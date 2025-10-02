use alloy::providers::Provider;
use std::sync::Arc;
use tokio::time::{interval, Duration};

pub struct EpochWatcher<P: Provider> {
    provider: Arc<P>,
}

impl<P: Provider> EpochWatcher<P> {
    pub fn new(provider: Arc<P>) -> Self {
        Self { provider }
    }

    pub async fn get_current_epoch(&self, epoch_period: u64) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let block = self.provider.get_block_by_number(Default::default()).await?.unwrap();
        Ok(block.header.timestamp / epoch_period)
    }

    pub async fn get_next_epoch_timestamp(&self, epoch_period: u64) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let current_epoch = self.get_current_epoch(epoch_period).await?;
        Ok((current_epoch + 1) * epoch_period)
    }

    pub async fn watch_epochs<F, Fut>(&self, epoch_period: u64, handler: F) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        F: Fn(u64) -> Fut + Send + 'static + Clone,
        Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send,
    {
        let mut check_interval = interval(Duration::from_secs(10));
        check_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_epoch = self.get_current_epoch(epoch_period).await?;

        loop {
            // Check immediately on first iteration, then wait for interval
            let current_epoch = self.get_current_epoch(epoch_period).await?;

            if current_epoch > last_epoch {
                let _ = handler(last_epoch).await;
                last_epoch = current_epoch;
            }

            check_interval.tick().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::providers::ProviderBuilder;

    #[tokio::test]
    async fn test_epoch_calculation() {
        dotenv::dotenv().ok();
        
        let rpc = std::env::var("ARBITRUM_RPC_URL")
            .expect("ARBITRUM_RPC_URL must be set");
        
        let provider = ProviderBuilder::new()
            .connect_http(rpc.parse().unwrap());
        let provider = Arc::new(provider);
        
        let watcher = EpochWatcher::new(provider);
        
        let epoch_period = 3600u64;
        let epoch = watcher.get_current_epoch(epoch_period).await.unwrap();
        assert!(epoch > 0, "Epoch should be non-zero");
        
        let next_timestamp = watcher.get_next_epoch_timestamp(epoch_period).await.unwrap();
        assert!(next_timestamp > epoch * epoch_period, "Next epoch timestamp should be in the future");
    }
}