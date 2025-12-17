use alloy::primitives::U256;
use alloy::providers::Provider;
use crate::config::Route;
use crate::contracts::IVeaInboxArbToEth;
use crate::tasks::send_tx;

pub async fn execute(
    route: &Route,
    epoch: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let inbox = IVeaInboxArbToEth::new(route.inbox_address, route.inbox_provider.clone());

    let epoch_period = inbox.epochPeriod().call().await?.to::<u64>();
    let epoch_start_ts = epoch * epoch_period;
    let current_block = route.inbox_provider.get_block_number().await?;
    let current_ts = route.inbox_provider.get_block_by_number(current_block.into()).await?.unwrap().header.timestamp;
    let elapsed_ms = current_ts.saturating_sub(epoch_start_ts) * 1000;
    let from_block = current_block.saturating_sub(elapsed_ms * 110 / 100 / (route.inbox_avg_block_millis as u64));

    let msg_sent_sig = alloy::primitives::keccak256("MessageSent(bytes)".as_bytes());
    let snapshot_saved_sig = alloy::primitives::keccak256("SnapshotSaved(bytes32,uint256,uint64)".as_bytes());

    let msg_filter = alloy::rpc::types::Filter::new()
        .address(route.inbox_address)
        .event_signature(msg_sent_sig)
        .from_block(from_block);
    let snapshot_filter = alloy::rpc::types::Filter::new()
        .address(route.inbox_address)
        .event_signature(snapshot_saved_sig)
        .from_block(from_block);

    let (msg_logs, snapshot_logs) = tokio::join!(
        route.inbox_provider.get_logs(&msg_filter),
        route.inbox_provider.get_logs(&snapshot_filter)
    );

    let msg_logs = msg_logs?;
    if msg_logs.is_empty() {
        return Ok(());
    }

    let snapshot_logs = snapshot_logs?;
    if let Some(last_snapshot) = snapshot_logs.last() {
        if last_snapshot.data().data.len() >= 96 {
            let saved_count = U256::from_be_slice(&last_snapshot.data().data[64..96]).to::<u64>();
            let current_count = inbox.count().call().await?;
            if saved_count == current_count {
                return Ok(());
            }
        }
    }

    send_tx(inbox.saveSnapshot().send().await, "saveSnapshot", route.name, &[]).await
}
