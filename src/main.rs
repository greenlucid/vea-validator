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
    config::ValidatorConfig,
};

/// Convert ClaimEvent to IVeaInboxArbToEth::Claim
fn claim_to_arb_eth(event: &ClaimEvent) -> IVeaInboxArbToEth::Claim {
    IVeaInboxArbToEth::Claim {
        stateRoot: event.state_root,
        claimer: event.claimer,
        timestampClaimed: event.timestamp_claimed,
        timestampVerification: 0,
        blocknumberVerification: 0,
        honest: IVeaInboxArbToEth::Party::None,
        challenger: Address::ZERO,
    }
}

/// Convert ClaimEvent to IVeaInboxArbToGnosis::Claim
fn claim_to_arb_gnosis(event: &ClaimEvent) -> IVeaInboxArbToGnosis::Claim {
    IVeaInboxArbToGnosis::Claim {
        stateRoot: event.state_root,
        claimer: event.claimer,
        timestampClaimed: event.timestamp_claimed,
        timestampVerification: 0,
        blocknumberVerification: 0,
        honest: IVeaInboxArbToGnosis::Party::None,
        challenger: Address::ZERO,
    }
}

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
            // Submit claim failure is OK - someone else may have claimed first
            // The Claimed event listener will verify whoever's claim
            if let Err(e) = handler.submit_claim(epoch, state_root).await {
                println!("[{}] Submit claim failed (likely someone else claimed first): {}", route, e);
            }
        }
        ClaimAction::Challenge { epoch, incorrect_claim } => {
            println!("[{}] Challenging incorrect claim for epoch {}", route, epoch);

            // Challenge the claim - acceptable to fail if someone else already challenged
            match handler.challenge_claim(epoch, make_claim(&incorrect_claim)).await {
                Ok(()) => {
                    println!("[{}] Challenge successful, triggering bridge resolution for epoch {}", route, epoch);

                    // Bridge resolution MUST succeed - failure means honest snapshot can't be verified
                    bridge_resolver(epoch, incorrect_claim.clone()).await
                        .unwrap_or_else(|e| panic!("[{}] FATAL: Failed to trigger bridge resolution for epoch {}: {}", route, epoch, e));

                    println!("[{}] Bridge resolution triggered successfully for epoch {}", route, epoch);
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    // OK if another validator already challenged - bridge is defended
                    if err_msg.contains("Claim already challenged") {
                        println!("[{}] Claim already challenged by another validator - bridge is safe", route);
                    } else {
                        // Any other error is FATAL - means we can't defend the bridge
                        panic!("[{}] FATAL: Failed to challenge incorrect claim for epoch {}: {}", route, epoch, e);
                    }
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
    let providers = vea_validator::config::setup_providers(destination_rpc.clone(), arbitrum_rpc.clone(), wallet)?;

    let claim_handler = Arc::new(ClaimHandler::new(
        providers.destination_with_wallet.clone(),
        providers.arbitrum_with_wallet.clone(),
        outbox_address,
        inbox_address,
        wallet_address,
        weth_address,
    ));

    let event_listener_inbox = EventListener::new(
        providers.arbitrum_provider.clone(),
        inbox_address,
    );

    let event_listener_outbox = EventListener::new(
        providers.destination_provider.clone(),
        outbox_address,
    );

    let epoch_watcher = EpochWatcher::new(
        providers.arbitrum_provider.clone(),
    );

    let inbox_contract = IVeaInboxArbToEth::new(inbox_address, providers.arbitrum_provider.clone());
    let epoch_period: u64 = inbox_contract.epochPeriod().call().await?.try_into()?;

    let current_epoch: u64 = inbox_contract.epochFinalized().call().await?.try_into()?;

    println!("[{}] Starting validator for route", route_name);
    println!("[{}] Inbox: {:?}, Outbox: {:?}", route_name, inbox_address, outbox_address);

    // Sync existing claims on startup - look back 10 epochs to be safe
    let sync_from = current_epoch.saturating_sub(10);
    println!("[{}] Syncing and verifying claims from epoch {} to {}...", route_name, sync_from, current_epoch);

    let startup_actions = claim_handler.startup_sync_and_verify(sync_from, current_epoch).await
        .unwrap_or_else(|e| panic!("[{}] FATAL: Failed to sync and verify claims: {}", route_name, e));

    // Handle all actions from startup sync
    for action in startup_actions {
        match &action {
            ClaimAction::Challenge { epoch, .. } => {
                println!("[{}] Found incorrect claim for epoch {} from startup sync - challenging", route_name, epoch);
            }
            _ => {}
        }
        let bridge_resolver_startup = bridge_resolver.clone();
        handle_claim_action(&claim_handler, action, route_name, &bridge_resolver_startup).await;
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
                let action = handler.handle_epoch_end(epoch).await
                    .unwrap_or_else(|e| panic!("[{}] FATAL: Failed to handle epoch end for epoch {}: {}", route, epoch, e));

                handle_claim_action(&handler, action, &route, &resolver).await;
                Ok(())
            })
        }).await
    });

    let claim_handler_for_snapshots = claim_handler.clone();
    let route_snapshot = route_name.to_string();
    let snapshot_handle = tokio::spawn(async move {
        event_listener_inbox.watch_snapshots(move |event: SnapshotEvent| {
            let handler = claim_handler_for_snapshots.clone();
            let route = route_snapshot.clone();
            Box::pin(async move {
                println!("[{}] Snapshot saved for epoch {} with root {:?}", route, event.epoch, event.state_root);

                // Check if a claim already exists
                match handler.get_claim_for_epoch(event.epoch).await {
                    Ok(Some(_existing_claim)) => {
                        println!("[{}] Claim already exists for epoch {} - was already verified when Claimed event fired", route, event.epoch);
                        // Do nothing - the claim was already verified and challenged (if needed) by the Claimed event listener
                    }
                    Ok(None) => {
                        println!("[{}] No claim exists for epoch {} - submitting honest claim", route, event.epoch);
                        // Note: Failure here is acceptable if someone else claims first
                        // The Claimed event listener will verify whoever's claim it is
                        if let Err(e) = handler.submit_claim(event.epoch, event.state_root).await {
                            println!("[{}] Submit claim failed (likely someone else claimed first): {}", route, e);
                        }
                    }
                    Err(e) => {
                        panic!("[{}] FATAL: Failed to query claim for epoch {}: {}", route, event.epoch, e);
                    }
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

                let action = handler.handle_claim_event(event.clone()).await
                    .unwrap_or_else(|e| panic!("[{}] FATAL: Failed to handle claim event for epoch {}: {}", route, event.epoch, e));

                handle_claim_action(&handler, action, &route, &resolver).await;
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
    let c = ValidatorConfig::from_env()?;

    let signer = PrivateKeySigner::from_str(&c.private_key)?;
    let wallet_address = signer.address();
    let wallet = EthereumWallet::from(signer);

    println!("Validator wallet address: {}", wallet_address);

    // Bridge resolver for ARB_TO_ETH: Direct bridge via Arbitrum canonical bridge
    let arb_to_eth_resolver = {
        let rpc = c.arbitrum_rpc.clone();
        let wlt = wallet.clone();
        let inbox = c.inbox_arb_to_eth;
        let wlt_addr = wallet_address;
        move |epoch: u64, claim: ClaimEvent| {
            let rpc = rpc.clone();
            let wlt = wlt.clone();
            async move {
                println!("[ARB_TO_ETH] Triggering bridge resolution for epoch {}", epoch);

                let provider = ProviderBuilder::<_, _, Ethereum>::new()
                    .wallet(wlt)
                    .connect_http(rpc.parse()?);
                let provider = Arc::new(provider);

                let inbox_contract = IVeaInboxArbToEth::new(inbox, provider);
                let tx = inbox_contract.sendSnapshot(U256::from(epoch), claim_to_arb_eth(&claim))
                    .from(wlt_addr);

                let pending = tx.send().await?;
                let receipt = pending.get_receipt().await?;

                if !receipt.status() {
                    return Err("sendSnapshot transaction failed".into());
                }

                println!("[ARB_TO_ETH] Bridge resolution triggered successfully for epoch {}", epoch);
                Ok(())
            }
        }
    };

    // Bridge resolver for ARB_TO_GNOSIS: Multi-hop via router
    // Note: The snapshot needs to be sent from Arbitrum, which goes to the router on mainnet,
    // which then forwards to Gnosis via AMB. This is a 2-hop process with ~7 day delay on first hop.
    let arb_to_gnosis_resolver = {
        let rpc = c.arbitrum_rpc.clone();
        let wlt = wallet.clone();
        let inbox = c.inbox_arb_to_gnosis;
        let wlt_addr = wallet_address;
        move |epoch: u64, claim: ClaimEvent| {
            let rpc = rpc.clone();
            let wlt = wlt.clone();
            async move {
                println!("[ARB_TO_GNOSIS] Triggering bridge resolution for epoch {}", epoch);

                let provider = ProviderBuilder::<_, _, Ethereum>::new()
                    .wallet(wlt)
                    .connect_http(rpc.parse()?);
                let provider = Arc::new(provider);

                let inbox_contract = IVeaInboxArbToGnosis::new(inbox, provider);

                // Gas limit for AMB message - using a reasonable default
                // This needs to be enough for the router to forward to Gnosis
                let gas_limit = U256::from(2_000_000u64);

                let tx = inbox_contract.sendSnapshot(U256::from(epoch), gas_limit, claim_to_arb_gnosis(&claim))
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
        }
    };

    let arb_to_eth_handle = tokio::spawn(run_validator_for_route(
        "ARB_TO_ETH",
        c.inbox_arb_to_eth,
        c.outbox_arb_to_eth,
        c.ethereum_rpc.clone(),
        c.arbitrum_rpc.clone(),
        wallet.clone(),
        wallet_address,
        None, // No WETH for ARB_TO_ETH route
        arb_to_eth_resolver,
    ));

    let arb_to_gnosis_handle = tokio::spawn(run_validator_for_route(
        "ARB_TO_GNOSIS",
        c.inbox_arb_to_gnosis,
        c.outbox_arb_to_gnosis,
        c.gnosis_rpc,
        c.arbitrum_rpc,
        wallet.clone(),
        wallet_address,
        Some(c.weth_gnosis), // WETH for ARB_TO_GNOSIS route
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
