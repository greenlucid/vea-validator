use alloy::primitives::{Address, U256};
use alloy::providers::{DynProvider, Provider};
use alloy::network::Ethereum;
use crate::contracts::IVeaInboxArbToEth;

pub async fn execute(
    inbox_provider: DynProvider<Ethereum>,
    inbox_address: Address,
    epoch: u64,
    route_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let inbox = IVeaInboxArbToEth::new(inbox_address, inbox_provider.clone());

    let epoch_period = inbox.epochPeriod().call().await?.to::<u64>();
    let epoch_start_ts = epoch * epoch_period;
    let current_block = inbox_provider.get_block_number().await?;
    let current_ts = inbox_provider.get_block_by_number(current_block.into()).await?.unwrap().header.timestamp;
    let elapsed_ms = (current_ts - epoch_start_ts) * 1000;
    let avg_block_millis: u64 = 250;
    let from_block = current_block.saturating_sub(elapsed_ms * 110 / 100 / avg_block_millis);

    let msg_sent_sig = alloy::primitives::keccak256("MessageSent(bytes)".as_bytes());
    let snapshot_saved_sig = alloy::primitives::keccak256("SnapshotSaved(bytes32,uint256,uint64)".as_bytes());

    let msg_filter = alloy::rpc::types::Filter::new()
        .address(inbox_address)
        .event_signature(msg_sent_sig)
        .from_block(from_block);
    let snapshot_filter = alloy::rpc::types::Filter::new()
        .address(inbox_address)
        .event_signature(snapshot_saved_sig)
        .from_block(from_block);

    let (msg_logs, snapshot_logs) = tokio::join!(
        inbox_provider.get_logs(&msg_filter),
        inbox_provider.get_logs(&snapshot_filter)
    );

    let msg_logs = msg_logs?;
    if msg_logs.is_empty() {
        println!("[{}] No messages in epoch {}, skipping saveSnapshot", route_name, epoch);
        return Ok(());
    }

    let snapshot_logs = snapshot_logs?;
    if let Some(last_snapshot) = snapshot_logs.last() {
        if last_snapshot.data().data.len() >= 96 {
            let saved_count = U256::from_be_slice(&last_snapshot.data().data[64..96]).to::<u64>();
            let current_count = inbox.count().call().await?;
            if saved_count == current_count {
                println!("[{}] Snapshot already saved for epoch {}", route_name, epoch);
                return Ok(());
            }
        }
    }

    println!("[{}] Calling saveSnapshot for epoch {}", route_name, epoch);
    let tx = inbox.saveSnapshot();
    let pending = tx.send().await?;
    let receipt = pending.get_receipt().await?;
    if !receipt.status() {
        return Err("saveSnapshot transaction failed".into());
    }
    println!("[{}] saveSnapshot succeeded for epoch {}", route_name, epoch);
    Ok(())
}
