use alloy::primitives::{Address, FixedBytes, U256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
const RELAY_DELAY: u64 = 7 * 24 * 3600 + 600;
#[derive(Debug, Clone)]
pub struct L2ToL1MessageData {
    pub ticket_id: FixedBytes<32>,
    pub position: U256,
    pub caller: Address,
    pub destination: Address,
    pub arb_block_num: U256,
    pub eth_block_num: U256,
    pub timestamp: u64,
    pub l2_timestamp: U256,
    pub callvalue: U256,
    pub data: Vec<u8>,
}
pub struct ProofRelay {
    pending: Arc<RwLock<HashMap<u64, L2ToL1MessageData>>>,
}
impl ProofRelay {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    pub async fn store_snapshot_sent(&self, epoch: u64, msg_data: L2ToL1MessageData) {
        let mut pending = self.pending.write().await;
        pending.insert(epoch, msg_data);
    }
    pub async fn watch_and_relay<F, Fut>(&self, handler: F) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        F: Fn(u64, L2ToL1MessageData) -> Fut + Send + 'static + Clone,
        Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send,
    {
        loop {
            sleep(Duration::from_secs(60)).await;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            let to_relay: Vec<(u64, L2ToL1MessageData)> = {
                let pending = self.pending.read().await;
                pending.iter()
                    .filter(|(_, msg)| now >= msg.timestamp + RELAY_DELAY)
                    .map(|(epoch, msg)| (*epoch, msg.clone()))
                    .collect()
            };
            for (epoch, msg_data) in to_relay {
                handler(epoch, msg_data).await?;
                let mut pending = self.pending.write().await;
                pending.remove(&epoch);
            }
        }
    }
}
