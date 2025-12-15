use alloy::providers::Provider;
use std::path::PathBuf;
use tokio::time::{sleep, Duration};

use crate::config::{Route, ValidatorConfig};
use crate::tasks;
use crate::tasks::{Task, TaskStore, ClaimStore};

const POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);

pub struct TaskDispatcher {
    config: ValidatorConfig,
    route: Route,
    task_store: TaskStore,
    claim_store: ClaimStore,
}

impl TaskDispatcher {
    pub fn new(
        config: ValidatorConfig,
        route: Route,
        schedule_path: impl Into<PathBuf>,
        claims_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            config,
            route,
            task_store: TaskStore::new(schedule_path),
            claim_store: ClaimStore::new(claims_path),
        }
    }

    pub async fn run(&self) {
        loop {
            self.process_pending().await;
            sleep(POLL_INTERVAL).await;
        }
    }

    pub async fn process_pending(&self) {
        let state = self.task_store.load();

        let now = match self.route.outbox_provider.get_block_by_number(Default::default()).await {
            Ok(Some(block)) => block.header.timestamp,
            _ => return,
        };

        let ready: Vec<Task> = state
            .tasks
            .iter()
            .filter(|t| now >= t.execute_after())
            .cloned()
            .collect();

        if ready.is_empty() {
            return;
        }

        println!("[{}][Dispatcher] Processing {} ready tasks", self.route.name, ready.len());

        for task in ready {
            let success = self.execute_task(&task, now).await;
            if success {
                self.task_store.remove_task(&task);
            }
        }
    }

    async fn execute_task(&self, task: &Task, current_timestamp: u64) -> bool {
        match task {
            Task::SaveSnapshot { epoch, .. } => {
                tasks::save_snapshot::execute(&self.route, *epoch).await.is_ok()
            }
            Task::Claim { epoch, .. } => {
                tasks::claim::execute(&self.route, *epoch).await.is_ok()
            }
            Task::VerifyClaim { epoch, .. } => {
                tasks::verify_claim::execute(
                    &self.route,
                    *epoch,
                    &self.claim_store,
                    current_timestamp,
                    &self.task_store,
                ).await.is_ok()
            }
            Task::Challenge { epoch, .. } => {
                tasks::challenge::execute(&self.route, *epoch, &self.claim_store).await.is_ok()
            }
            Task::SendSnapshot { epoch, .. } => {
                tasks::send_snapshot::execute(&self.route, *epoch, &self.claim_store).await.is_ok()
            }
            Task::StartVerification { epoch, .. } => {
                tasks::start_verification::execute(&self.route, *epoch, &self.claim_store).await.is_ok()
            }
            Task::VerifySnapshot { epoch, .. } => {
                tasks::verify_snapshot::execute(&self.route, *epoch, &self.claim_store).await.is_ok()
            }
            Task::ExecuteRelay { position, l2_sender, dest_addr, l2_block, l1_block, l2_timestamp, amount, data, .. } => {
                tasks::execute_relay::execute(
                    &self.route,
                    self.config.arb_outbox,
                    *position,
                    *l2_sender,
                    *dest_addr,
                    *l2_block,
                    *l1_block,
                    *l2_timestamp,
                    *amount,
                    data.clone(),
                ).await.is_ok()
            }
        }
    }
}
