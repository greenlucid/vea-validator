use alloy::primitives::{Address, U256, FixedBytes};
use alloy::providers::{ProviderBuilder, Provider};
use alloy::signers::local::PrivateKeySigner;
use alloy::network::{Ethereum, EthereumWallet};
use std::str::FromStr;
use std::sync::Arc;
use vea_validator::{
    event_listener::{EventListener, ClaimEvent, SnapshotSentEvent},
    epoch_watcher::EpochWatcher,
    claim_handler::{ClaimHandler, ClaimAction, make_claim},
    contracts::{IVeaInboxArbToEth, IVeaInboxArbToGnosis, IArbSys, IOutbox},
    config::ValidatorConfig,
    proof_relay::{ProofRelay, L2ToL1MessageData},
    startup::{check_rpc_health, check_balances},
};

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

async fn handle_claim_action<F, Fut>(
    handler: &Arc<ClaimHandler>,
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
    route: vea_validator::config::Route,
    wallet: EthereumWallet,
    bridge_resolver: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    F: Fn(u64, ClaimEvent) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send,
{
    let claim_handler = Arc::new(ClaimHandler::new(
        route.inbox_rpc.clone(),
        route.outbox_rpc.clone(),
        route.inbox_address,
        route.outbox_address,
        wallet.clone(),
        route.weth_address,
    ));
    let event_listener_outbox = EventListener::new(
        route.outbox_rpc.clone(),
        route.outbox_address,
    );
    let event_listener_inbox = EventListener::new(
        route.inbox_rpc.clone(),
        route.inbox_address,
    );
    let proof_relay = Arc::new(ProofRelay::new());
    let epoch_watcher = EpochWatcher::new(
        route.inbox_rpc.clone(),
    );
    let inbox_provider = ProviderBuilder::new().connect_http(route.inbox_rpc.parse()?);
    let inbox_contract = IVeaInboxArbToEth::new(route.inbox_address, inbox_provider);
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
                let action = handler.handle_claim_event(event.clone()).await
                    .unwrap_or_else(|e| panic!("[{}] FATAL: Failed to handle claim event for epoch {}: {}", route.name, event.epoch, e));
                handle_claim_action(&handler, action, route.name, &resolver).await;
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
    let inbox_rpc_for_relay = route.inbox_rpc.clone();
    let outbox_rpc_for_relay = route.outbox_rpc.clone();
    let outbox_address_for_relay = route.outbox_address;
    let wallet_for_relay = wallet.clone();
    let relay_handle = tokio::spawn(async move {
        proof_relay.watch_and_relay(move |epoch, msg_data| {
            let inbox_rpc = inbox_rpc_for_relay.clone();
            let outbox_rpc = outbox_rpc_for_relay.clone();
            let wlt = wallet_for_relay.clone();
            Box::pin(async move {
                println!("[{}] Executing proof relay for epoch {}", route.name, epoch);
                let inbox_provider = ProviderBuilder::new().connect_http(inbox_rpc.parse()?);
                let arb_sys = IArbSys::new(Address::from_slice(&[0u8; 19].iter().chain(&[0x64u8]).copied().collect::<Vec<u8>>()), inbox_provider);
                let merkle_state = arb_sys.sendMerkleTreeState().call().await.map_err(|e| format!("Failed to get merkle state: {}", e))?;
                let size = merkle_state.size;
                let node_interface_addr = Address::from_slice(&[0u8; 19].iter().chain(&[0xC8u8]).copied().collect::<Vec<u8>>());
                let proof_bytes = {
                    use alloy::rpc::types::TransactionRequest;
                    use alloy::primitives::Bytes;
                    let inbox_provider = ProviderBuilder::new().connect_http(inbox_rpc.parse()?);
                    let mut call_data = vec![0x42, 0x69, 0x6c, 0x6c];
                    call_data.extend_from_slice(&size.to_be_bytes::<32>());
                    call_data.extend_from_slice(&msg_data.position.to_be_bytes::<32>());
                    let result = inbox_provider.call(TransactionRequest::default()
                        .to(node_interface_addr)
                        .input(Bytes::from(call_data).into())).await.map_err(|e| format!("NodeInterface call failed: {}", e))?;
                    result
                };
                if proof_bytes.len() < 96 {
                    return Err(format!("Invalid proof response length: {}", proof_bytes.len()).into());
                }
                let proof_array_offset = U256::from_be_slice(&proof_bytes[64..96]).to::<usize>();
                let proof_start = 96 + proof_array_offset;
                if proof_bytes.len() < proof_start + 32 {
                    return Err("Proof data truncated".into());
                }
                let proof_len = U256::from_be_slice(&proof_bytes[proof_start..proof_start + 32]).to::<usize>();
                let mut proof: Vec<FixedBytes<32>> = Vec::new();
                for i in 0..proof_len {
                    let offset = proof_start + 32 + (i * 32);
                    if proof_bytes.len() < offset + 32 {
                        break;
                    }
                    proof.push(FixedBytes::<32>::from_slice(&proof_bytes[offset..offset + 32]));
                }
                let outbox_provider = ProviderBuilder::new().wallet(wlt).connect_http(outbox_rpc.parse()?);
                let outbox = IOutbox::new(outbox_address_for_relay, outbox_provider);
                let tx = outbox.executeTransaction(
                    proof,
                    msg_data.position,
                    msg_data.caller,
                    msg_data.destination,
                    msg_data.arb_block_num,
                    msg_data.eth_block_num,
                    msg_data.l2_timestamp,
                    msg_data.callvalue,
                    alloy::primitives::Bytes::from(msg_data.data)
                );
                let receipt = tx.send().await.map_err(|e| format!("executeTransaction send failed: {}", e))?.get_receipt().await.map_err(|e| format!("Receipt fetch failed: {}", e))?;
                if !receipt.status() {
                    return Err(format!("executeTransaction failed for epoch {}", epoch).into());
                }
                println!("[{}] Proof relay executed successfully for epoch {}", route.name, epoch);
                Ok(())
            })
        }).await
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
    check_rpc_health(&c).await?;
    check_balances(&c).await?;
    let routes = c.build_routes();
    let arb_to_eth_route = &routes[0];
    let arb_to_gnosis_route = &routes[1];

    let arb_to_eth_resolver = {
        let rpc = arb_to_eth_route.inbox_rpc.clone();
        let wlt = wallet.clone();
        let inbox = arb_to_eth_route.inbox_address;
        move |epoch: u64, claim: ClaimEvent| {
            let rpc = rpc.clone();
            let wlt = wlt.clone();
            async move {
                println!("[ARB_TO_ETH] Triggering bridge resolution for epoch {}", epoch);
                let provider = ProviderBuilder::<_, _, Ethereum>::new()
                    .wallet(wlt.clone())
                    .connect_http(rpc.parse()?);
                let inbox_contract = IVeaInboxArbToEth::new(inbox, provider);
                let tx = inbox_contract.sendSnapshot(U256::from(epoch), claim_to_arb_eth(&claim))
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
        let rpc = arb_to_gnosis_route.inbox_rpc.clone();
        let wlt = wallet.clone();
        let inbox = arb_to_gnosis_route.inbox_address;
        move |epoch: u64, claim: ClaimEvent| {
            let rpc = rpc.clone();
            let wlt = wlt.clone();
            async move {
                println!("[ARB_TO_GNOSIS] Triggering bridge resolution for epoch {}", epoch);
                let provider = ProviderBuilder::<_, _, Ethereum>::new()
                    .wallet(wlt.clone())
                    .connect_http(rpc.parse()?);
                let inbox_contract = IVeaInboxArbToGnosis::new(inbox, provider);
                let gas_limit = U256::from(2_000_000u64);
                let tx = inbox_contract.sendSnapshot(U256::from(epoch), gas_limit, claim_to_arb_gnosis(&claim))
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
