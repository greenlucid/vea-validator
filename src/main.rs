use alloy::primitives::{Address, U256};
use alloy::providers::ProviderBuilder;
use alloy::signers::local::PrivateKeySigner;
use alloy::network::{Ethereum, EthereumWallet};
use std::str::FromStr;
use std::sync::Arc;
use vea_validator::{
    event_listener::{EventListener, SnapshotEvent, ClaimEvent},
    epoch_watcher::EpochWatcher,
    claim_handler::{ClaimHandler, ClaimAction, make_claim},
    contracts::{IVeaInboxArbToEth, IVeaInboxArbToGnosis},
};

async fn handle_claim_action<P: alloy::providers::Provider, F, Fut>(
    handler: &Arc<ClaimHandler<P>>,
    action: ClaimAction,
    route: &str,
    bridge_resolver: &F,
) where
    F: Fn(u64, ClaimEvent) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send,
{
    match action {
        ClaimAction::None => {},
        ClaimAction::Claim { epoch, state_root } => {
            println!("[{}] Submitting claim for epoch {}", route, epoch);
            let _ = handler.submit_claim(epoch, state_root).await;
        }
        ClaimAction::Challenge { epoch, incorrect_claim } => {
            println!("[{}] Challenging claim for epoch {}", route, epoch);
            if let Err(e) = handler.challenge_claim(epoch, make_claim(&incorrect_claim)).await {
                eprintln!("[{}] Failed to challenge claim: {}", route, e);
            } else {
                // After successfully challenging, trigger bridge resolution
                println!("[{}] Challenge successful, triggering bridge resolution for epoch {}", route, epoch);
                if let Err(e) = bridge_resolver(epoch, incorrect_claim.clone()).await {
                    eprintln!("[{}] Failed to trigger bridge resolution: {}", route, e);
                }
            }
        }
    }
}

async fn run_validator_for_route<F, Fut>(
    route_name: &str,
    inbox_address: Address,
    outbox_address: Address,
    destination_rpc: String,
    arbitrum_rpc: String,
    wallet: EthereumWallet,
    wallet_address: Address,
    weth_address: Option<Address>,
    bridge_resolver: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    F: Fn(u64, ClaimEvent) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send,
{
    let destination_provider = ProviderBuilder::new()
        .connect_http(destination_rpc.parse()?);
    let destination_provider = Arc::new(destination_provider);

    let arbitrum_provider = ProviderBuilder::new()
        .connect_http(arbitrum_rpc.parse()?);
    let arbitrum_provider = Arc::new(arbitrum_provider);

    let destination_provider_with_wallet = ProviderBuilder::<_, _, Ethereum>::new()
        .wallet(wallet.clone())
        .connect_provider(destination_provider.clone());
    let destination_provider_with_wallet = Arc::new(destination_provider_with_wallet);

    let arbitrum_provider_with_wallet = ProviderBuilder::<_, _, Ethereum>::new()
        .wallet(wallet)
        .connect_provider(arbitrum_provider.clone());
    let arbitrum_provider_with_wallet = Arc::new(arbitrum_provider_with_wallet);

    let claim_handler = Arc::new(ClaimHandler::new(
        destination_provider_with_wallet.clone(),
        arbitrum_provider_with_wallet.clone(),
        outbox_address,
        inbox_address,
        wallet_address,
        weth_address,
    ));

    let event_listener_inbox = EventListener::new(
        arbitrum_provider.clone(),
        inbox_address,
    );

    let event_listener_outbox = EventListener::new(
        destination_provider.clone(),
        outbox_address,
    );

    let epoch_watcher = EpochWatcher::new(
        arbitrum_provider.clone(),
    );

    let inbox_contract = IVeaInboxArbToEth::new(inbox_address, arbitrum_provider.clone());
    let epoch_period: u64 = inbox_contract.epochPeriod().call().await?.try_into()?;

    let current_epoch: u64 = inbox_contract.epochFinalized().call().await?.try_into()?;

    println!("[{}] Starting validator for route", route_name);
    println!("[{}] Inbox: {:?}, Outbox: {:?}", route_name, inbox_address, outbox_address);

    // Sync existing claims on startup - look back 10 epochs to be safe
    let sync_from = current_epoch.saturating_sub(10);
    if let Err(e) = claim_handler.sync_existing_claims(sync_from, current_epoch).await {
        eprintln!("[{}] Warning: Failed to sync existing claims: {}", route_name, e);
    }

    let claim_handler_for_epoch = claim_handler.clone();
    let route_epoch = route_name.to_string();
    let bridge_resolver_epoch = bridge_resolver.clone();
    let epoch_handle = tokio::spawn(async move {
        epoch_watcher.watch_epochs(epoch_period, move |epoch| {
            let handler = claim_handler_for_epoch.clone();
            let route = route_epoch.clone();
            let resolver = bridge_resolver_epoch.clone();
            Box::pin(async move {
                if let Ok(action) = handler.handle_epoch_end(epoch).await {
                    handle_claim_action(&handler, action, &route, &resolver).await;
                }
                Ok(())
            })
        }).await
    });

    let claim_handler_for_snapshots = claim_handler.clone();
    let route_snapshot = route_name.to_string();
    let bridge_resolver_snapshot = bridge_resolver.clone();
    let snapshot_handle = tokio::spawn(async move {
        event_listener_inbox.watch_snapshots(move |event: SnapshotEvent| {
            let handler = claim_handler_for_snapshots.clone();
            let route = route_snapshot.clone();
            let resolver = bridge_resolver_snapshot.clone();
            Box::pin(async move {
                println!("[{}] Snapshot saved for epoch {} with root {:?}", route, event.epoch, event.state_root);

                match handler.get_claim_for_epoch(event.epoch).await {
                    Ok(Some(existing_claim)) => {
                        println!("[{}] Claim already exists for epoch {}", route, event.epoch);
                        match handler.verify_claim(&existing_claim).await {
                            Ok(true) => println!("[{}] Existing claim is valid", route),
                            Ok(false) => {
                                println!("[{}] Existing claim is INVALID - need to challenge", route);
                                if let Err(e) = handler.challenge_claim(event.epoch, make_claim(&existing_claim)).await {
                                    eprintln!("[{}] Failed to challenge claim: {}", route, e);
                                } else {
                                    println!("[{}] Challenge successful, triggering bridge resolution for epoch {}", route, event.epoch);
                                    if let Err(e) = resolver(event.epoch, existing_claim.clone()).await {
                                        eprintln!("[{}] Failed to trigger bridge resolution: {}", route, e);
                                    }
                                }
                            }
                            Err(e) => eprintln!("[{}] Error verifying claim: {}", route, e),
                        }
                    }
                    Ok(None) => {
                        println!("[{}] No claim for epoch {} - submitting claim", route, event.epoch);
                        if let Err(e) = handler.submit_claim(event.epoch, event.state_root).await {
                            eprintln!("[{}] Failed to submit claim: {}", route, e);
                        }
                    }
                    Err(e) => eprintln!("[{}] Error checking claim: {}", route, e),
                }
                Ok(())
            })
        }).await
    });

    let claim_handler_for_claims = claim_handler.clone();
    let route_claim = route_name.to_string();
    let bridge_resolver_claim = bridge_resolver.clone();
    let claim_handle = tokio::spawn(async move {
        event_listener_outbox.watch_claims(move |event: ClaimEvent| {
            let handler = claim_handler_for_claims.clone();
            let route = route_claim.clone();
            let resolver = bridge_resolver_claim.clone();
            Box::pin(async move {
                println!("[{}] Claim detected for epoch {} by {}", route, event.epoch, event.claimer);

                if let Ok(action) = handler.handle_claim_event(event.clone()).await {
                    handle_claim_action(&handler, action, &route, &resolver).await;
                }
                Ok(())
            })
        }).await
    });

    tokio::select! {
        _ = epoch_handle => println!("[{}] Epoch watcher stopped", route_name),
        _ = snapshot_handle => println!("[{}] Snapshot watcher stopped", route_name),
        _ = claim_handle => println!("[{}] Claim watcher stopped", route_name),
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv::dotenv().ok();

    let arbitrum_rpc = std::env::var("ARBITRUM_RPC_URL")
        .expect("ARBITRUM_RPC_URL must be set");

    let private_key = std::env::var("PRIVATE_KEY")
        .or_else(|_| std::fs::read_to_string("/run/secrets/validator_key")
            .map(|s| s.trim().to_string()))
        .expect("PRIVATE_KEY not set or /run/secrets/validator_key not found");

    let signer = PrivateKeySigner::from_str(&private_key)?;
    let wallet_address = signer.address();
    let wallet = EthereumWallet::from(signer);

    println!("Validator wallet address: {}", wallet_address);

    // ARB_TO_ETH route setup
    let inbox_arb_to_eth = Address::from_str(
        &std::env::var("VEA_INBOX_ARB_TO_ETH")
            .expect("VEA_INBOX_ARB_TO_ETH must be set")
    )?;
    let outbox_arb_to_eth = Address::from_str(
        &std::env::var("VEA_OUTBOX_ARB_TO_ETH")
            .expect("VEA_OUTBOX_ARB_TO_ETH must be set")
    )?;
    let ethereum_rpc = std::env::var("ETHEREUM_RPC_URL")
        .or_else(|_| std::env::var("MAINNET_RPC_URL"))
        .expect("ETHEREUM_RPC_URL or MAINNET_RPC_URL must be set");

    // ARB_TO_GNOSIS route setup
    let inbox_arb_to_gnosis = Address::from_str(
        &std::env::var("VEA_INBOX_ARB_TO_GNOSIS")
            .expect("VEA_INBOX_ARB_TO_GNOSIS must be set")
    )?;
    let outbox_arb_to_gnosis = Address::from_str(
        &std::env::var("VEA_OUTBOX_ARB_TO_GNOSIS")
            .expect("VEA_OUTBOX_ARB_TO_GNOSIS must be set")
    )?;
    let gnosis_rpc = std::env::var("GNOSIS_RPC_URL")
        .expect("GNOSIS_RPC_URL must be set");
    let weth_gnosis = Address::from_str(
        &std::env::var("WETH_GNOSIS")
            .expect("WETH_GNOSIS must be set")
    )?;

    // Bridge resolver for ARB_TO_ETH: Direct bridge via Arbitrum canonical bridge
    let arb_rpc_for_eth = arbitrum_rpc.clone();
    let wallet_for_eth = wallet.clone();
    let inbox_addr_eth = inbox_arb_to_eth;
    let wallet_addr_eth = wallet_address;
    let arb_to_eth_resolver = move |epoch: u64, claim: ClaimEvent| {
        let rpc = arb_rpc_for_eth.clone();
        let wlt = wallet_for_eth.clone();
        let inbox = inbox_addr_eth;
        let wlt_addr = wallet_addr_eth;
        async move {
            println!("[ARB_TO_ETH] Triggering bridge resolution for epoch {}", epoch);

            let provider = ProviderBuilder::<_, _, Ethereum>::new()
                .wallet(wlt)
                .connect_http(rpc.parse()?);
            let provider = Arc::new(provider);

            let inbox_contract = IVeaInboxArbToEth::new(inbox, provider);

            // Convert ClaimEvent to the Claim struct expected by sendSnapshot
            let outbox_claim = IVeaInboxArbToEth::Claim {
                stateRoot: claim.state_root,
                claimer: claim.claimer,
                timestampClaimed: claim.timestamp_claimed,
                timestampVerification: 0,
                blocknumberVerification: 0,
                honest: IVeaInboxArbToEth::Party::None,
                challenger: Address::ZERO,
            };

            let tx = inbox_contract.sendSnapshot(U256::from(epoch), outbox_claim)
                .from(wlt_addr);

            let pending = tx.send().await?;
            let receipt = pending.get_receipt().await?;

            if !receipt.status() {
                return Err("sendSnapshot transaction failed".into());
            }

            println!("[ARB_TO_ETH] Bridge resolution triggered successfully for epoch {}", epoch);
            Ok(())
        }
    };

    // Bridge resolver for ARB_TO_GNOSIS: Multi-hop via router
    // Note: The snapshot needs to be sent from Arbitrum, which goes to the router on mainnet,
    // which then forwards to Gnosis via AMB. This is a 2-hop process with ~7 day delay on first hop.
    let arb_rpc_for_gnosis = arbitrum_rpc.clone();
    let wallet_for_gnosis = wallet.clone();
    let inbox_addr_gnosis = inbox_arb_to_gnosis;
    let wallet_addr_gnosis = wallet_address;
    let arb_to_gnosis_resolver = move |epoch: u64, claim: ClaimEvent| {
        let rpc = arb_rpc_for_gnosis.clone();
        let wlt = wallet_for_gnosis.clone();
        let inbox = inbox_addr_gnosis;
        let wlt_addr = wallet_addr_gnosis;
        async move {
            println!("[ARB_TO_GNOSIS] Triggering bridge resolution for epoch {}", epoch);

            let provider = ProviderBuilder::<_, _, Ethereum>::new()
                .wallet(wlt)
                .connect_http(rpc.parse()?);
            let provider = Arc::new(provider);

            let inbox_contract = IVeaInboxArbToGnosis::new(inbox, provider);

            // Convert ClaimEvent to the Claim struct expected by sendSnapshot
            let outbox_claim = IVeaInboxArbToGnosis::Claim {
                stateRoot: claim.state_root,
                claimer: claim.claimer,
                timestampClaimed: claim.timestamp_claimed,
                timestampVerification: 0,
                blocknumberVerification: 0,
                honest: IVeaInboxArbToGnosis::Party::None,
                challenger: Address::ZERO,
            };

            // Gas limit for AMB message - using a reasonable default
            // This needs to be enough for the router to forward to Gnosis
            let gas_limit = U256::from(2_000_000u64);

            let tx = inbox_contract.sendSnapshot(U256::from(epoch), gas_limit, outbox_claim)
                .from(wlt_addr);

            let pending = tx.send().await?;
            let receipt = pending.get_receipt().await?;

            if !receipt.status() {
                return Err("sendSnapshot transaction failed".into());
            }

            println!("[ARB_TO_GNOSIS] Bridge resolution triggered successfully for epoch {}", epoch);
            println!("[ARB_TO_GNOSIS] Note: Message will take ~7 days to reach mainnet, then be forwarded to Gnosis");
            Ok(())
        }
    };

    let arb_to_eth_handle = tokio::spawn(run_validator_for_route(
        "ARB_TO_ETH",
        inbox_arb_to_eth,
        outbox_arb_to_eth,
        ethereum_rpc,
        arbitrum_rpc.clone(),
        wallet.clone(),
        wallet_address,
        None, // No WETH for ARB_TO_ETH route
        arb_to_eth_resolver,
    ));

    let arb_to_gnosis_handle = tokio::spawn(run_validator_for_route(
        "ARB_TO_GNOSIS",
        inbox_arb_to_gnosis,
        outbox_arb_to_gnosis,
        gnosis_rpc,
        arbitrum_rpc,
        wallet.clone(),
        wallet_address,
        Some(weth_gnosis), // WETH for ARB_TO_GNOSIS route
        arb_to_gnosis_resolver,
    ));

    println!("Running validators for both ARB_TO_ETH and ARB_TO_GNOSIS routes simultaneously...");

    tokio::select! {
        _ = arb_to_eth_handle => println!("ARB_TO_ETH validator stopped"),
        _ = arb_to_gnosis_handle => println!("ARB_TO_GNOSIS validator stopped"),
        _ = tokio::signal::ctrl_c() => println!("\nShutting down..."),
    }

    Ok(())
}
