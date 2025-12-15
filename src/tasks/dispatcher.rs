use alloy::primitives::Address;
use alloy::providers::{DynProvider, Provider};
use alloy::network::Ethereum;
use std::path::PathBuf;
use tokio::time::{sleep, Duration};

use crate::tasks;
use crate::tasks::{Task, TaskStore};

const POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);

pub struct TaskDispatcher {
    inbox_provider: DynProvider<Ethereum>,
    inbox_address: Address,
    outbox_provider: DynProvider<Ethereum>,
    outbox_address: Address,
    arb_outbox_address: Address,
    weth_address: Option<Address>,
    wallet_address: Address,
    task_store: TaskStore,
    route_name: &'static str,
}

impl TaskDispatcher {
    pub fn new(
        inbox_provider: DynProvider<Ethereum>,
        inbox_address: Address,
        outbox_provider: DynProvider<Ethereum>,
        outbox_address: Address,
        arb_outbox_address: Address,
        weth_address: Option<Address>,
        wallet_address: Address,
        schedule_path: impl Into<PathBuf>,
        route_name: &'static str,
    ) -> Self {
        Self {
            inbox_provider,
            inbox_address,
            outbox_provider,
            outbox_address,
            arb_outbox_address,
            weth_address,
            wallet_address,
            task_store: TaskStore::new(schedule_path),
            route_name,
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

        let now = match self.outbox_provider.get_block_by_number(Default::default()).await {
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

        println!("[{}][Dispatcher] Processing {} ready tasks", self.route_name, ready.len());

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
                tasks::save_snapshot::execute(
                    self.inbox_provider.clone(),
                    self.inbox_address,
                    *epoch,
                    self.route_name,
                ).await.is_ok()
            }
            Task::Claim { epoch, .. } => {
                tasks::claim::execute(
                    self.inbox_provider.clone(),
                    self.inbox_address,
                    self.outbox_provider.clone(),
                    self.outbox_address,
                    *epoch,
                    self.route_name,
                ).await.is_ok()
            }
            Task::VerifyClaim { epoch, state_root, claimer, timestamp_claimed, .. } => {
                tasks::verify_claim::execute(
                    self.inbox_provider.clone(),
                    self.inbox_address,
                    *epoch,
                    *state_root,
                    *claimer,
                    *timestamp_claimed,
                    current_timestamp,
                    &self.task_store,
                    self.route_name,
                ).await.is_ok()
            }
            Task::Challenge { epoch, state_root, claimer, timestamp_claimed, .. } => {
                tasks::challenge::execute(
                    self.outbox_provider.clone(),
                    self.outbox_address,
                    self.weth_address,
                    self.wallet_address,
                    *epoch,
                    *state_root,
                    *claimer,
                    *timestamp_claimed,
                    self.route_name,
                ).await.is_ok()
            }
            Task::SendSnapshot { epoch, state_root, claimer, timestamp_claimed, challenger, .. } => {
                tasks::send_snapshot::execute(
                    self.inbox_provider.clone(),
                    self.inbox_address,
                    self.weth_address,
                    *epoch,
                    *state_root,
                    *claimer,
                    *timestamp_claimed,
                    *challenger,
                    self.route_name,
                ).await.is_ok()
            }
            Task::StartVerification { epoch, state_root, claimer, timestamp_claimed, .. } => {
                tasks::start_verification::execute(
                    self.outbox_provider.clone(),
                    self.outbox_address,
                    *epoch,
                    *state_root,
                    *claimer,
                    *timestamp_claimed,
                    self.route_name,
                ).await.is_ok()
            }
            Task::VerifySnapshot { epoch, state_root, claimer, timestamp_claimed, timestamp_verification, blocknumber_verification, .. } => {
                tasks::verify_snapshot::execute(
                    self.outbox_provider.clone(),
                    self.outbox_address,
                    *epoch,
                    *state_root,
                    *claimer,
                    *timestamp_claimed,
                    *timestamp_verification,
                    *blocknumber_verification,
                    self.route_name,
                ).await.is_ok()
            }
            Task::ExecuteRelay { epoch, position, l2_sender, dest_addr, l2_block, l1_block, l2_timestamp, amount, data, .. } => {
                tasks::execute_relay::execute(
                    self.inbox_provider.clone(),
                    self.outbox_provider.clone(),
                    self.arb_outbox_address,
                    *epoch,
                    *position,
                    *l2_sender,
                    *dest_addr,
                    *l2_block,
                    *l1_block,
                    *l2_timestamp,
                    *amount,
                    data.clone(),
                    self.route_name,
                ).await.is_ok()
            }
        }
    }
}
