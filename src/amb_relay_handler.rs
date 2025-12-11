use alloy::primitives::Address;
use alloy::providers::DynProvider;
use alloy::network::Ethereum;
use std::path::PathBuf;
use tokio::time::{sleep, Duration};

use crate::contracts::IAMB;
use crate::scheduler::{AmbTask, ScheduleFile};

const POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);

pub struct AmbRelayHandler {
    gnosis_provider: DynProvider<Ethereum>,
    amb_address: Address,
    schedule_path: PathBuf,
}

impl AmbRelayHandler {
    pub fn new(
        gnosis_provider: DynProvider<Ethereum>,
        amb_address: Address,
        schedule_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            gnosis_provider,
            amb_address,
            schedule_path: schedule_path.into(),
        }
    }

    pub async fn run(&self) {
        loop {
            self.process_pending().await;
            sleep(POLL_INTERVAL).await;
        }
    }

    pub async fn process_pending(&self) {
        let schedule_file: ScheduleFile<AmbTask> = ScheduleFile::new(&self.schedule_path);
        let mut schedule = schedule_file.load();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let ready: Vec<AmbTask> = schedule
            .pending
            .iter()
            .filter(|t| now >= t.execute_after)
            .cloned()
            .collect();

        if ready.is_empty() {
            return;
        }

        println!("[AmbRelayHandler] Checking {} pending AMB messages", ready.len());

        let amb = IAMB::new(self.amb_address, self.gnosis_provider.clone());

        for task in ready {
            schedule.pending.retain(|t| t.ticket_id != task.ticket_id);

            match amb.messageCallStatus(task.ticket_id).call().await {
                Ok(is_executed) if is_executed => {
                    println!(
                        "[AmbRelayHandler] Epoch {} AMB message already executed",
                        task.epoch
                    );
                }
                Ok(_) => {
                    eprintln!(
                        "[AmbRelayHandler] WARNING: Epoch {} AMB message NOT executed after delay! ticket_id={:#x}",
                        task.epoch, task.ticket_id
                    );
                }
                Err(e) => {
                    eprintln!(
                        "[AmbRelayHandler] Failed to check messageCallStatus for epoch {}: {}",
                        task.epoch, e
                    );
                }
            }
        }

        schedule_file.save(&schedule);
    }
}
