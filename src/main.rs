use alloy::primitives::U256;
use alloy::signers::local::PrivateKeySigner;
use alloy::network::EthereumWallet;
use std::str::FromStr;
use std::sync::Arc;
use vea_validator::{
    event_listener::{EventListener, ClaimEvent, SnapshotSentEvent},
    epoch_watcher::EpochWatcher,
    claim_handler::{ClaimHandler, make_claim},
    contracts::{IVeaInboxArbToEth, IVeaInboxArbToGnosis},
    config::ValidatorConfig,
    proof_relay::{ProofRelay, L2ToL1MessageData},
    startup::{check_rpc_health, check_balances},
};

async fn handle_invalid_claim<F, Fut>(
    handler: &Arc<ClaimHandler>,
    incorrect_claim: ClaimEvent,
    route: &str,
    bridge_resolver: &F,
) where
    F: Fn(u64, ClaimEvent) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send,
{
    let epoch = incorrect_claim.epoch;
    println!("[{}] Challenging incorrect claim for epoch {}", route, epoch);
    match handler.challenge_claim(epoch, make_claim(&incorrect_claim)).await {
        Ok(()) => {
            println!("[{}] Challenge successful, triggering bridge resolution for epoch {}", route, epoch);
            bridge_resolver(epoch, incorrect_claim.clone()).await
                .unwrap_or_else(|e| panic!("[{}] FATAL: Failed to trigger bridge resolution for epoch {}: {}", route, epoch, e));
            println!("[{}] Bridge resolution triggered successfully for epoch {}", route, epoch);
        }
        Err(_) => {
            println!("[{}] Claim already challenged by another validator - bridge is safe", route);
        }
    }
}

async fn run_validator_for_route<F, Fut>(
    route: vea_validator::config::Route,
    wallet: EthereumWallet,
    bridge_resolver: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    F: Fn(u64, ClaimEvent) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send,
{
    let claim_handler = Arc::new(ClaimHandler::new(
        route.clone(),
        wallet.clone(),
    ));
    let event_listener_outbox = EventListener::new(
        route.outbox_provider.clone(),
        route.outbox_address,
    );
    let event_listener_inbox = EventListener::new(
        route.inbox_provider.clone(),
        route.inbox_address,
    );
    let proof_relay = Arc::new(ProofRelay::new(route.clone(), wallet.clone()));
    let epoch_watcher = EpochWatcher::new(
        route.inbox_provider.clone(),
    );
    let inbox_contract = IVeaInboxArbToEth::new(route.inbox_address, route.inbox_provider.clone());
    let epoch_period: u64 = inbox_contract.epochPeriod().call().await?.try_into()?;
    println!("[{}] Starting validator for route", route.name);
    println!("[{}] Inbox: {:?}, Outbox: {:?}", route.name, route.inbox_address, route.outbox_address);
    let claim_handler_for_epochs_before = claim_handler.clone();
    let claim_handler_for_epochs_after = claim_handler.clone();
    let epoch_handle = tokio::spawn(async move {
        epoch_watcher.watch_epochs(
            epoch_period,
            move |epoch| {
                let handler = claim_handler_for_epochs_before.clone();
                Box::pin(async move {
                    handler.handle_epoch_end(epoch).await
                        .unwrap_or_else(|e| panic!("[{}] FATAL: Failed to save snapshot for epoch {}: {}", route.name, epoch, e));
                    Ok(())
                })
            },
            move |epoch| {
                let handler = claim_handler_for_epochs_after.clone();
                Box::pin(async move {
                    handler.handle_after_epoch_start(epoch).await
                        .unwrap_or_else(|e| panic!("[{}] FATAL: Failed to handle after epoch start for epoch {}: {}", route.name, epoch, e));
                    Ok(())
                })
            },
        ).await
    });
    let claim_handler_for_claims = claim_handler.clone();
    let bridge_resolver_for_claims = bridge_resolver.clone();
    let claim_handle = tokio::spawn(async move {
        event_listener_outbox.watch_claims(move |event: ClaimEvent| {
            let handler = claim_handler_for_claims.clone();
            let resolver = bridge_resolver_for_claims.clone();
            Box::pin(async move {
                println!("[{}] Claim detected for epoch {} by {}", route.name, event.epoch, event.claimer);
                let invalid_claim = handler.handle_claim_event(event.clone()).await
                    .unwrap_or_else(|e| panic!("[{}] FATAL: Failed to handle claim event for epoch {}: {}", route.name, event.epoch, e));
                if let Some(claim) = invalid_claim {
                    handle_invalid_claim(&handler, claim, route.name, &resolver).await;
                }
                Ok(())
            })
        }).await
    });
    let proof_relay_for_snapshot = proof_relay.clone();
    let snapshot_sent_handle = tokio::spawn(async move {
        event_listener_inbox.watch_snapshot_sent(move |event: SnapshotSentEvent| {
            let relay = proof_relay_for_snapshot.clone();
            Box::pin(async move {
                println!("[{}] SnapshotSent for epoch {} with ticketID {:?}", route.name, event.epoch, event.ticket_id);
                let msg_data = L2ToL1MessageData {
                    ticket_id: event.ticket_id,
                    position: event.position,
                    caller: event.caller,
                    destination: event.destination,
                    arb_block_num: event.arb_block_num,
                    eth_block_num: event.eth_block_num,
                    timestamp: event.timestamp,
                    l2_timestamp: event.l2_timestamp,
                    callvalue: event.callvalue,
                    data: event.data,
                };
                relay.store_snapshot_sent(event.epoch, msg_data).await;
                Ok(())
            })
        }).await
    });
    let relay_handle = tokio::spawn(async move {
        proof_relay.watch_and_relay().await
    });
    tokio::select! {
        _ = epoch_handle => println!("[{}] Epoch watcher stopped", route.name),
        _ = claim_handle => println!("[{}] Claim watcher stopped", route.name),
        _ = snapshot_sent_handle => println!("[{}] SnapshotSent watcher stopped", route.name),
        _ = relay_handle => println!("[{}] Proof relay stopped", route.name),
    }
    Ok(())
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let c = ValidatorConfig::from_env()?;
    let signer = PrivateKeySigner::from_str(&c.private_key)?;
    let wallet = EthereumWallet::from(signer);
    println!("Validator wallet address: {}", wallet.default_signer().address());
    let routes = c.build_routes();
    check_rpc_health(&routes).await?;
    check_balances(&c, &routes).await?;
    let arb_to_eth_route = &routes[0];
    let arb_to_gnosis_route = &routes[1];

    let arb_to_eth_resolver = {
        let provider = arb_to_eth_route.inbox_provider.clone();
        let wlt = wallet.clone();
        let inbox = arb_to_eth_route.inbox_address;
        move |epoch: u64, claim: ClaimEvent| {
            let provider = provider.clone();
            let wlt = wlt.clone();
            async move {
                println!("[ARB_TO_ETH] Triggering bridge resolution for epoch {}", epoch);
                let inbox_contract = IVeaInboxArbToEth::new(inbox, provider);
                let tx = inbox_contract.sendSnapshot(U256::from(epoch), make_claim(&claim))
                    .from(wlt.default_signer().address());
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
        let provider = arb_to_gnosis_route.inbox_provider.clone();
        let wlt = wallet.clone();
        let inbox = arb_to_gnosis_route.inbox_address;
        move |epoch: u64, claim: ClaimEvent| {
            let provider = provider.clone();
            let wlt = wlt.clone();
            async move {
                println!("[ARB_TO_GNOSIS] Triggering bridge resolution for epoch {}", epoch);
                let inbox_contract = IVeaInboxArbToGnosis::new(inbox, provider);
                let gas_limit = U256::from(2_000_000u64);
                let tx = inbox_contract.sendSnapshot(U256::from(epoch), gas_limit, make_claim(&claim))
                    .from(wlt.default_signer().address());
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
        routes[0].clone(),
        wallet.clone(),
        arb_to_eth_resolver,
    ));
    let arb_to_gnosis_handle = tokio::spawn(run_validator_for_route(
        routes[1].clone(),
        wallet.clone(),
        arb_to_gnosis_resolver,
    ));
    println!("Running validators for both ARB_TO_ETH and ARB_TO_GNOSIS routes simultaneously...");

    // Monitor both routes - if one fails, log but continue with the other
    let monitor_handle = tokio::spawn(async move {
        let (eth_result, gnosis_result) = tokio::join!(arb_to_eth_handle, arb_to_gnosis_handle);

        match eth_result {
            Ok(Ok(())) => println!("ARB_TO_ETH validator stopped gracefully"),
            Ok(Err(e)) => eprintln!("ARB_TO_ETH validator failed: {}", e),
            Err(e) => eprintln!("ARB_TO_ETH validator panicked: {}", e),
        }

        match gnosis_result {
            Ok(Ok(())) => println!("ARB_TO_GNOSIS validator stopped gracefully"),
            Ok(Err(e)) => eprintln!("ARB_TO_GNOSIS validator failed: {}", e),
            Err(e) => eprintln!("ARB_TO_GNOSIS validator panicked: {}", e),
        }
    });

    tokio::select! {
        _ = monitor_handle => println!("Both routes stopped"),
        _ = tokio::signal::ctrl_c() => println!("\nShutting down..."),
    }
    Ok(())
}
