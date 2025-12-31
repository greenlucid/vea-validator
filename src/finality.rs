use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::rpc::types::{BlockNumberOrTag, Filter};
use alloy::network::Ethereum;
use alloy::providers::DynProvider;

use crate::contracts::INodeInterface;
use crate::find_block_by_timestamp;

const NODE_INTERFACE: Address = Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xC8]);
const BATCH_POSTING_DELAY_SECS: u64 = 5 * 60;
const SEARCH_RANGE_BLOCKS: u64 = 1000;

pub async fn is_epoch_finalized(
    epoch: u64,
    epoch_period: u64,
    arb_provider: &DynProvider<Ethereum>,
    l1_provider: &DynProvider<Ethereum>,
    sequencer_inbox: Option<Address>,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let sequencer_inbox = match sequencer_inbox {
        Some(addr) => addr,
        None => return Ok(true),
    };

    let epoch_end_ts = (epoch + 1) * epoch_period;
    let l2_block = find_block_by_timestamp(arb_provider, epoch_end_ts).await;

    let batch = get_batch_for_block(arb_provider, l2_block).await?;

    let l1_block = get_l1_block_for_batch(l1_provider, sequencer_inbox, batch, epoch_end_ts).await?;
    if l1_block.is_none() {
        return Ok(false);
    }
    let l1_block = l1_block.unwrap();

    let finalized = l1_provider.get_block_by_number(BlockNumberOrTag::Finalized).await?;
    match finalized {
        Some(block) => Ok(block.header.number >= l1_block),
        None => Ok(false),
    }
}

async fn get_batch_for_block(
    arb_provider: &DynProvider<Ethereum>,
    l2_block: u64,
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let node_interface = INodeInterface::new(NODE_INTERFACE, arb_provider.clone());
    let result = node_interface.findBatchContainingBlock(l2_block).call().await?;
    Ok(result)
}

async fn get_l1_block_for_batch(
    l1_provider: &DynProvider<Ethereum>,
    sequencer_inbox: Address,
    batch: u64,
    epoch_end_ts: u64,
) -> Result<Option<u64>, Box<dyn std::error::Error + Send + Sync>> {
    let event_sig = alloy::primitives::keccak256("SequencerBatchDelivered(uint256,bytes32,bytes32,bytes32,uint256,(uint64,uint64,uint64,uint64),uint8)");

    let latest = l1_provider.get_block_number().await?;
    let midpoint = find_block_by_timestamp(l1_provider, epoch_end_ts + BATCH_POSTING_DELAY_SECS).await;
    let from_block = midpoint.saturating_sub(SEARCH_RANGE_BLOCKS);
    let to_block = (midpoint + SEARCH_RANGE_BLOCKS).min(latest);

    let filter = Filter::new()
        .address(sequencer_inbox)
        .event_signature(event_sig)
        .topic1(U256::from(batch))
        .from_block(from_block)
        .to_block(to_block);

    let logs = l1_provider.get_logs(&filter).await?;

    if logs.is_empty() {
        return Ok(None);
    }

    Ok(logs[0].block_number)
}
