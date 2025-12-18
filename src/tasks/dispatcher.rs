use alloy::providers::Provider;
use std::sync::{Arc, Mutex};
use tokio::time::{sleep, Duration};

use crate::config::{Route, ValidatorConfig};
use crate::tasks;
use crate::tasks::{Task, TaskKind, TaskStore, ClaimStore};

const POLL_INTERVAL: Duration = Duration::from_secs(15);

pub struct TaskDispatcher {
    config: ValidatorConfig,
    route: Route,
    task_store: Arc<Mutex<TaskStore>>,
    claim_store: Arc<Mutex<ClaimStore>>,
}

impl TaskDispatcher {
    pub fn new(
        config: ValidatorConfig,
        route: Route,
        task_store: Arc<Mutex<TaskStore>>,
        claim_store: Arc<Mutex<ClaimStore>>,
    ) -> Self {
        Self {
            config,
            route,
            task_store,
            claim_store,
        }
    }

    pub async fn run(&self) {
        loop {
            self.process_pending().await;
            sleep(POLL_INTERVAL).await;
        }
    }

    pub async fn process_pending(&self) {
        if !self.task_store.lock().unwrap().is_on_sync() {
            return;
        }

        let state = self.task_store.lock().unwrap().load();

        let now = self.route.outbox_provider.get_block_by_number(Default::default()).await
            .expect("Failed to get latest block")
            .expect("Latest block not found")
            .header.timestamp;

        let ready: Vec<Task> = state
            .tasks
            .iter()
            .filter(|t| now >= t.execute_after)
            .cloned()
            .collect();

        if ready.is_empty() {
            return;
        }

        println!("[{}][Dispatcher] Processing {} ready tasks", self.route.name, ready.len());

        for task in ready {
            println!("[{}][Dispatcher] Executing {} for epoch {}", self.route.name, task.kind.name(), task.epoch);
            let success = self.execute_task(&task, now).await;
            if success {
                println!("[{}][Dispatcher] Completed {} for epoch {}", self.route.name, task.kind.name(), task.epoch);
                self.task_store.lock().unwrap().remove_task(&task);
            }
        }
    }

    async fn execute_task(&self, task: &Task, current_timestamp: u64) -> bool {
        let epoch = task.epoch;
        match &task.kind {
            TaskKind::SaveSnapshot => {
                tasks::save_snapshot::execute(&self.route, epoch).await.is_ok()
            }
            TaskKind::Claim { .. } => {
                tasks::claim::execute(&self.route, epoch, &self.claim_store, current_timestamp).await.is_ok()
            }
            TaskKind::ValidateClaim => {
                tasks::validate_claim::execute(
                    &self.route,
                    epoch,
                    &self.claim_store,
                    current_timestamp,
                    &self.task_store,
                ).await.is_ok()
            }
            TaskKind::Challenge => {
                match tasks::challenge::execute(&self.config, &self.route, epoch, &self.claim_store).await {
                    Ok(_) => true,
                    Err(e) if e.to_string() == "Insufficient funds" => {
                        self.task_store.lock().unwrap().reschedule_task(task, current_timestamp + 15 * 60);
                        true
                    }
                    Err(e) if e.to_string() == "VerificationStarted" => {
                        self.task_store.lock().unwrap().reschedule_task(task, current_timestamp + 15 * 60);
                        true
                    }
                    Err(e) if e.to_string().contains("Invalid claim") => {
                        self.task_store.lock().unwrap().reschedule_task(task, current_timestamp + 30 * 60);
                        true
                    }
                    Err(_) => false,
                }
            }
            TaskKind::SendSnapshot => {
                tasks::send_snapshot::execute(&self.route, epoch, &self.claim_store).await.is_ok()
            }
            TaskKind::StartVerification => {
                match tasks::start_verification::execute(&self.route, epoch, &self.claim_store).await {
                    Ok(_) => true,
                    Err(e) if e.to_string().contains("Invalid claim") => {
                        self.task_store.lock().unwrap().reschedule_task(task, current_timestamp + 30 * 60);
                        true
                    }
                    Err(_) => false,
                }
            }
            TaskKind::VerifySnapshot => {
                match tasks::verify_snapshot::execute(&self.route, epoch, &self.claim_store).await {
                    Ok(_) => true,
                    Err(e) if e.to_string().contains("Invalid claim") => {
                        self.task_store.lock().unwrap().reschedule_task(task, current_timestamp + 30 * 60);
                        true
                    }
                    Err(_) => false,
                }
            }
            TaskKind::ExecuteRelay { position, l2_sender, dest_addr, l2_block, l1_block, l2_timestamp, amount, data } => {
                match tasks::execute_relay::execute(
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
                ).await {
                    Ok(_) => true,
                    Err(e) if e.to_string() == "RootNotConfirmed" => {
                        self.task_store.lock().unwrap().reschedule_task(task, current_timestamp + 60 * 60);
                        true
                    }
                    Err(_) => false,
                }
            }
            TaskKind::WithdrawDeposit => {
                tasks::withdraw_deposit::execute(&self.route, epoch, &self.claim_store).await.is_ok()
            }
        }
    }
}
