pub mod contracts;
pub mod config;
pub mod startup;
pub mod tasks;
pub mod epoch_watcher;
pub mod indexer;
pub mod finality;

use alloy::providers::{Provider, DynProvider};
use alloy::network::Ethereum;

pub async fn find_block_by_timestamp(provider: &DynProvider<Ethereum>, target_ts: u64) -> u64 {
    let latest = provider.get_block_number().await.expect("Failed to get latest block number");
    let latest_block = provider.get_block_by_number(latest.into()).await
        .expect("Failed to get latest block")
        .expect("Latest block not found");

    if target_ts >= latest_block.header.timestamp {
        return latest;
    }

    let mut lo = 0u64;
    let mut hi = latest;

    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let block = provider.get_block_by_number(mid.into()).await
            .expect("Failed to get block during binary search")
            .expect("Block not found during binary search");
        if block.header.timestamp < target_ts {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}
