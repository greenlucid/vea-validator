pub mod contracts;
pub mod config;
pub mod startup;
pub mod tasks;
pub mod epoch_watcher;
pub mod indexer;
pub mod finality;

use alloy::providers::{Provider, DynProvider};
use alloy::network::Ethereum;
use alloy::rpc::types::Block;
use tokio::time::{sleep, Duration};
use tracing::warn;

const RPC_RETRIES: u32 = 3;
const RPC_RETRY_DELAY: Duration = Duration::from_secs(2);

async fn get_block_with_retry(provider: &DynProvider<Ethereum>, block_num: alloy::eips::BlockNumberOrTag) -> Block {
    for attempt in 1..=RPC_RETRIES {
        match provider.get_block_by_number(block_num).await {
            Ok(Some(block)) => return block,
            Ok(None) => panic!("Block {block_num} not found"),
            Err(e) => {
                if attempt == RPC_RETRIES {
                    panic!("Failed to get block {block_num} after {RPC_RETRIES} attempts: {e}");
                }
                warn!(logger = "RPC", attempt, "Failed to get block {block_num}: {e}, retrying...");
                sleep(RPC_RETRY_DELAY).await;
            }
        }
    }
    unreachable!()
}

pub async fn find_block_by_timestamp(provider: &DynProvider<Ethereum>, target_ts: u64) -> u64 {
    let latest = get_block_with_retry(provider, Default::default()).await;
    let latest_num = latest.header.number;

    if target_ts >= latest.header.timestamp {
        return latest_num;
    }

    let mut lo = 0u64;
    let mut hi = latest_num;

    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let block = get_block_with_retry(provider, mid.into()).await;
        if block.header.timestamp < target_ts {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}
