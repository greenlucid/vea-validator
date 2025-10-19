use alloy::primitives::FixedBytes;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
const RELAY_DELAY: u64 = 7 * 24 * 3600 + 600;
pub struct ProofRelay {
    pending: Arc<RwLock<HashMap<u64, (FixedBytes<32>, u64)>>>,
}
impl ProofRelay {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    pub async fn store_snapshot_sent(&self, epoch: u64, ticket_id: FixedBytes<32>, timestamp: u64) {
        let mut pending = self.pending.write().await;
        pending.insert(epoch, (ticket_id, timestamp));
    }
    pub async fn watch_and_relay<F, Fut>(&self, handler: F) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        F: Fn(u64, FixedBytes<32>) -> Fut + Send + 'static + Clone,
        Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send,
    {
        loop {
            sleep(Duration::from_secs(60)).await;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            let to_relay: Vec<(u64, FixedBytes<32>)> = {
                let pending = self.pending.read().await;
                pending.iter()
                    .filter(|(_, (_, timestamp))| now >= timestamp + RELAY_DELAY)
                    .map(|(epoch, (ticket_id, _))| (*epoch, *ticket_id))
                    .collect()
            };
            for (epoch, ticket_id) in to_relay {
                handler(epoch, ticket_id).await?;
                let mut pending = self.pending.write().await;
                pending.remove(&epoch);
            }
        }
    }
}
