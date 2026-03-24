use std::sync::{Arc, Mutex};
use alloy::providers::Provider;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};

use crate::config::{Route, ValidatorConfig};
use crate::tasks;
use crate::tasks::{ClaimStore, TaskStore};

const BEFORE_EPOCH_BUFFER: u64 = 60;
const AFTER_EPOCH_BUFFER: u64 = 20 * 60;

pub struct EpochWatcher {
    config: ValidatorConfig,
    route: Route,
    make_claims: bool,
    claim_store: Arc<Mutex<ClaimStore>>,
    task_store: Arc<Mutex<TaskStore>>,
}

impl EpochWatcher {
    pub fn new(config: ValidatorConfig, route: Route, make_claims: bool, claim_store: Arc<Mutex<ClaimStore>>, task_store: Arc<Mutex<TaskStore>>) -> Self {
        Self {
            config,
            route,
            make_claims,
            claim_store,
            task_store,
        }
    }

    async fn get_current_timestamp(&self) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let block = self.route.inbox_provider.get_block_by_number(Default::default()).await?.unwrap();
        Ok(block.header.timestamp)
    }

    pub async fn watch_epochs(&self, epoch_period: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut last_before_epoch: Option<u64> = None;
        let mut last_after_epoch: Option<u64> = None;
        let mut skip_claim_until: u64 = 0;
        loop {
            let now = match self.get_current_timestamp().await {
                Ok(ts) => ts,
                Err(e) => {
                    warn!(logger = "EpochWatcher", route = self.route.name, "Failed to get timestamp: {e}");
                    sleep(Duration::from_secs(10)).await;
                    continue;
                }
            };
            let current_epoch = now / epoch_period;
            let next_epoch_start = (current_epoch + 1) * epoch_period;
            let time_until_next_epoch = next_epoch_start.saturating_sub(now);

            if self.task_store.lock().unwrap().is_on_sync() && time_until_next_epoch <= BEFORE_EPOCH_BUFFER && last_before_epoch != Some(current_epoch) {
                tasks::save_snapshot::execute(&self.route, &self.task_store).await
                    .unwrap_or_else(|e| panic!("[{}] FATAL: Failed to save snapshot for epoch {}: {}", self.route.name, current_epoch, e));
                last_before_epoch = Some(current_epoch);
            }

            if self.make_claims && self.task_store.lock().unwrap().is_on_sync() && now >= skip_claim_until {
                let time_since_epoch_start = now.saturating_sub(current_epoch * epoch_period);
                if time_since_epoch_start >= AFTER_EPOCH_BUFFER && current_epoch > 0 {
                    let prev_epoch = current_epoch - 1;
                    if last_after_epoch != Some(prev_epoch) {
                        info!(logger = "EpochWatcher", route = self.route.name, epoch = prev_epoch, "Checking claim");
                        match tasks::claim::execute(&self.config, &self.route, prev_epoch, &self.claim_store, now).await {
                            Ok(()) => { last_after_epoch = Some(prev_epoch); }
                            Err(e) if e.to_string() == "EpochNotFinalized" => {
                                skip_claim_until = now + 300;
                            }
                            Err(e) if e.to_string().contains("timed out") || e.to_string().contains("connection") => {
                            warn!(logger = "EpochWatcher", route = self.route.name, epoch = prev_epoch, "Claim RPC error: {e}, retrying next cycle");
                        }
                        Err(e) => panic!("[{}] FATAL: Failed to claim epoch {}: {}", self.route.name, prev_epoch, e),
                        }
                    }
                }
            }

            sleep(Duration::from_secs(10)).await;
        }
    }
}
