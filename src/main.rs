use alloy::primitives::{Address, U256};
use alloy::providers::{ProviderBuilder, Provider};
use alloy::signers::local::PrivateKeySigner;
use alloy::network::{Ethereum, EthereumWallet};
use std::str::FromStr;
use std::sync::Arc;
use vea_validator::{
    event_listener::{EventListener, ClaimEvent, SnapshotSentEvent},
    epoch_watcher::EpochWatcher,
    claim_handler::{ClaimHandler, ClaimAction, make_claim},
    contracts::{IVeaInboxArbToEth, IVeaInboxArbToGnosis, IVeaOutboxArbToEth, IVeaOutboxArbToGnosis, IWETH},
    config::ValidatorConfig,
    proof_relay::ProofRelay,
};
async fn check_balances(c: &ValidatorConfig, wallet: Address) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let signer = PrivateKeySigner::from_str(&c.private_key)?;
    let eth_providers = vea_validator::config::setup_providers(c.ethereum_rpc.clone(), c.arbitrum_rpc.clone(), EthereumWallet::from(signer.clone()))?;
    let gnosis_providers = vea_validator::config::setup_providers(c.gnosis_rpc.clone(), c.arbitrum_rpc.clone(), EthereumWallet::from(signer))?;
    let eth_outbox = IVeaOutboxArbToEth::new(c.outbox_arb_to_eth, eth_providers.destination_provider.clone());
    let gnosis_outbox = IVeaOutboxArbToGnosis::new(c.outbox_arb_to_gnosis, gnosis_providers.destination_provider.clone());
    let eth_deposit = eth_outbox.deposit().call().await?;
    let eth_balance = eth_providers.destination_provider.get_balance(wallet).await?;
    if eth_balance < eth_deposit {
        panic!("FATAL: Insufficient ETH balance. Need {} wei for deposit, have {} wei", eth_deposit, eth_balance);
    }
    let gnosis_deposit = gnosis_outbox.deposit().call().await?;
    let weth = IWETH::new(c.weth_gnosis, gnosis_providers.destination_provider.clone());
    let weth_balance = weth.balanceOf(wallet).call().await?;
    if weth_balance < gnosis_deposit {
        panic!("FATAL: Insufficient WETH balance on Gnosis. Need {} wei for deposit, have {} wei", gnosis_deposit, weth_balance);
    }
    println!("✓ Balance check passed: ETH={} wei, WETH={} wei", eth_balance, weth_balance);
    Ok(())
}
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
            if let Err(e) = handler.submit_claim(epoch, state_root).await {
                println!("[{}] Submit claim failed (likely someone else claimed first): {}", route, e);
            }
        }
        ClaimAction::Challenge { epoch, incorrect_claim } => {
            println!("[{}] Challenging incorrect claim for epoch {}", route, epoch);
            match handler.challenge_claim(epoch, make_claim(&incorrect_claim)).await {
                Ok(()) => {
                    println!("[{}] Challenge successful, triggering bridge resolution for epoch {}", route, epoch);
                    bridge_resolver(epoch, incorrect_claim.clone()).await
                        .unwrap_or_else(|e| panic!("[{}] FATAL: Failed to trigger bridge resolution for epoch {}: {}", route, epoch, e));
                    println!("[{}] Bridge resolution triggered successfully for epoch {}", route, epoch);
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    if err_msg.contains("Claim already challenged") {
                        println!("[{}] Claim already challenged by another validator - bridge is safe", route);
                    } else {
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
    let event_listener_outbox = EventListener::new(
        providers.destination_provider.clone(),
        outbox_address,
    );
    let event_listener_inbox = EventListener::new(
        providers.arbitrum_provider.clone(),
        inbox_address,
    );
    let proof_relay = Arc::new(ProofRelay::new());
    let epoch_watcher = EpochWatcher::new(
        providers.arbitrum_provider.clone(),
    );
    let inbox_contract = IVeaInboxArbToEth::new(inbox_address, providers.arbitrum_provider.clone());
    let epoch_period: u64 = inbox_contract.epochPeriod().call().await?.try_into()?;
    println!("[{}] Starting validator for route", route_name);
    println!("[{}] Inbox: {:?}, Outbox: {:?}", route_name, inbox_address, outbox_address);
    let claim_handler_before = claim_handler.clone();
    let route_before = route_name.to_string();
    let claim_handler_after = claim_handler.clone();
    let route_after = route_name.to_string();
    let epoch_handle = tokio::spawn(async move {
        epoch_watcher.watch_epochs(
            epoch_period,
            move |epoch| {
                let handler = claim_handler_before.clone();
                let route = route_before.clone();
                Box::pin(async move {
                    handler.handle_epoch_end(epoch).await
                        .unwrap_or_else(|e| panic!("[{}] FATAL: Failed to save snapshot for epoch {}: {}", route, epoch, e));
                    Ok(())
                })
            },
            move |epoch| {
                let handler = claim_handler_after.clone();
                let route = route_after.clone();
                Box::pin(async move {
                    match handler.get_correct_state_root(epoch).await {
                        Ok(state_root) if state_root != alloy::primitives::FixedBytes::<32>::ZERO => {
                            match handler.get_claim_for_epoch(epoch).await {
                                Ok(Some(_)) => {
                                    println!("[{}] Claim already exists for epoch {}", route, epoch);
                                }
                                Ok(None) => {
                                    println!("[{}] No claim for epoch {}, submitting", route, epoch);
                                    if let Err(e) = handler.submit_claim(epoch, state_root).await {
                                        println!("[{}] Submit claim failed (likely someone else claimed first): {}", route, e);
                                    }
                                }
                                Err(e) => {
                                    panic!("[{}] FATAL: Failed to query claim for epoch {}: {}", route, epoch, e);
                                }
                            }
                        }
                        Ok(_) => {
                            println!("[{}] No snapshot saved for epoch {}, skipping claim", route, epoch);
                        }
                        Err(e) => {
                            panic!("[{}] FATAL: Failed to get state root for epoch {}: {}", route, epoch, e);
                        }
                    }
                    Ok(())
                })
            },
        ).await
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
    let proof_relay_for_snapshot_sent = proof_relay.clone();
    let route_snapshot_sent = route_name.to_string();
    let snapshot_sent_handle = tokio::spawn(async move {
        event_listener_inbox.watch_snapshot_sent(move |event: SnapshotSentEvent| {
            let relay = proof_relay_for_snapshot_sent.clone();
            let route = route_snapshot_sent.clone();
            Box::pin(async move {
                println!("[{}] SnapshotSent for epoch {} with ticketID {:?}", route, event.epoch, event.ticket_id);
                relay.store_snapshot_sent(event.epoch, event.ticket_id, event.timestamp).await;
                Ok(())
            })
        }).await
    });
    let route_relay = route_name.to_string();
    let relay_handle = tokio::spawn(async move {
        proof_relay.watch_and_relay(move |epoch, ticket_id| {
            let route = route_relay.clone();
            Box::pin(async move {
                println!("[{}] TODO: Relay proof for epoch {} with ticketID {:?}", route, epoch, ticket_id);
                println!("[{}] Need to implement: constructOutboxProof({:?}) then Outbox.executeTransaction()", route, ticket_id);
                Ok(())
            })
        }).await
    });
    tokio::select! {
        _ = epoch_handle => println!("[{}] Epoch watcher stopped", route_name),
        _ = claim_handle => println!("[{}] Claim watcher stopped", route_name),
        _ = snapshot_sent_handle => println!("[{}] SnapshotSent watcher stopped", route_name),
        _ = relay_handle => println!("[{}] Proof relay stopped", route_name),
    }
    Ok(())
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let c = ValidatorConfig::from_env()?;
    let signer = PrivateKeySigner::from_str(&c.private_key)?;
    let wallet_address = signer.address();
    let wallet = EthereumWallet::from(signer);
    println!("Validator wallet address: {}", wallet_address);
    check_balances(&c, wallet_address).await?;
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
                let tx_result = tx.send().await?;
                let receipt = tx_result.get_receipt().await?;
                if !receipt.status() {
                    return Err("sendSnapshot transaction failed".into());
                }
                println!("[ARB_TO_ETH] Bridge resolution triggered successfully for epoch {}", epoch);
                Ok(())
            }
        }
    };
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
                let gas_limit = U256::from(2_000_000u64);
                let tx = inbox_contract.sendSnapshot(U256::from(epoch), gas_limit, claim_to_arb_gnosis(&claim))
                    .from(wlt_addr);
                let tx_result = tx.send().await?;
                let receipt = tx_result.get_receipt().await?;
                if !receipt.status() {
                    return Err("sendSnapshot transaction failed".into());
                }
                println!("[ARB_TO_GNOSIS] Bridge resolution triggered successfully for epoch {}", epoch);
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
        None,
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
        Some(c.weth_gnosis),
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
