use tokio::time::{sleep, Duration};
const BEFORE_EPOCH_BUFFER: u64 = 300;
const AFTER_EPOCH_BUFFER: u64 = 60;
pub struct EpochWatcher {
    rpc_url: String,
}
impl EpochWatcher {
    pub fn new(rpc_url: String) -> Self {
        Self { rpc_url }
    }
    async fn get_current_timestamp(&self) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        use alloy::providers::{Provider, ProviderBuilder};
        let provider = ProviderBuilder::new().connect_http(self.rpc_url.parse()?);
        let block = provider.get_block_by_number(Default::default()).await?.unwrap();
        Ok(block.header.timestamp)
    }
    pub async fn watch_epochs<FB, FA, FutB, FutA>(
        &self,
        epoch_period: u64,
        before_handler: FB,
        after_handler: FA,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        FB: Fn(u64) -> FutB + Send + 'static + Clone,
        FutB: std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send,
        FA: Fn(u64) -> FutA + Send + 'static + Clone,
        FutA: std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send,
    {
        let mut last_before_epoch: Option<u64> = None;
        let mut last_after_epoch: Option<u64> = None;
        loop {
            let now = self.get_current_timestamp().await?;
            let current_epoch = now / epoch_period;
            let next_epoch_start = (current_epoch + 1) * epoch_period;
            let time_until_next_epoch = next_epoch_start.saturating_sub(now);
            if time_until_next_epoch <= BEFORE_EPOCH_BUFFER
                && last_before_epoch != Some(current_epoch) {
                before_handler(current_epoch).await?;
                last_before_epoch = Some(current_epoch);
            }
            let time_since_epoch_start = now.saturating_sub(current_epoch * epoch_period);
            if time_since_epoch_start >= AFTER_EPOCH_BUFFER && current_epoch > 0 {
                let prev_epoch = current_epoch - 1;
                if last_after_epoch != Some(prev_epoch) {
                    after_handler(prev_epoch).await?;
                    last_after_epoch = Some(prev_epoch);
                }
            }
            sleep(Duration::from_secs(10)).await;
        }
    }
}
