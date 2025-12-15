use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use alloy::providers::Provider;
use alloy::rpc::types::Filter;
use std::path::PathBuf;
use std::cmp::min;
use tokio::time::{sleep, Duration};

use crate::config::Route;
use crate::contracts::{IVeaInbox, IVeaOutboxArbToEth, IVeaOutboxArbToGnosis};
use crate::tasks::{self, Task, TaskStore, ClaimStore, ClaimData, send_snapshot};

const CHUNK_SIZE: u64 = 500;
const FINALITY_BUFFER_SECS: u64 = 15 * 60;
const CATCHUP_SLEEP: Duration = Duration::from_secs(5);
const IDLE_SLEEP: Duration = Duration::from_secs(5 * 60);
const RELAY_DELAY: u64 = 7 * 24 * 3600 + 3600;
const ARB_SYS: Address = Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x64]);

pub struct EventIndexer {
    route: Route,
    task_store: TaskStore,
    claim_store: ClaimStore,
}

impl EventIndexer {
    pub fn new(
        route: Route,
        schedule_path: impl Into<PathBuf>,
        claims_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            route,
            task_store: TaskStore::new(schedule_path),
            claim_store: ClaimStore::new(claims_path),
        }
    }

    pub async fn run(&self) {
        loop {
            let done = self.scan_once().await;
            if done {
                sleep(IDLE_SLEEP).await;
            } else {
                sleep(CATCHUP_SLEEP).await;
            }
        }
    }

    pub async fn scan_once(&self) -> bool {
        let inbox_done = self.scan_inbox().await;
        let outbox_done = self.scan_outbox().await;
        inbox_done && outbox_done
    }

    async fn scan_inbox(&self) -> bool {
        let snapshot_sent_sig = alloy::primitives::keccak256("SnapshotSent(uint256,bytes32)");

        let current_block = match self.route.inbox_provider.get_block_number().await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[{}][Indexer] Failed to get inbox block number: {}", self.route.name, e);
                return true;
            }
        };

        let current_block_data = match self.route.inbox_provider.get_block_by_number(current_block.into()).await {
            Ok(Some(b)) => b,
            _ => return true,
        };
        let now = current_block_data.header.timestamp;

        let state = self.task_store.load();
        let ten_days_blocks = 10 * 24 * 3600 * 1000 / 250;
        let from_block = state.inbox_last_block.unwrap_or_else(|| current_block.saturating_sub(ten_days_blocks));

        if from_block >= current_block {
            return true;
        }

        let to_block = min(from_block + CHUNK_SIZE, current_block);

        let filter = Filter::new()
            .address(self.route.inbox_address)
            .event_signature(snapshot_sent_sig)
            .from_block(from_block)
            .to_block(to_block);

        match self.route.inbox_provider.get_logs(&filter).await {
            Ok(logs) => {
                for log in logs {
                    let block_ts = log.block_timestamp.unwrap_or(0);
                    if block_ts > now.saturating_sub(FINALITY_BUFFER_SECS) {
                        continue;
                    }
                    self.handle_snapshot_sent(&log).await;
                }
                self.task_store.update_inbox_block(to_block);
                println!(
                    "[{}][Indexer] Inbox scanned blocks {}-{}",
                    self.route.name, from_block, to_block
                );
                to_block >= current_block
            }
            Err(e) => {
                eprintln!(
                    "[{}][Indexer] Failed to query inbox logs {}-{}: {}",
                    self.route.name, from_block, to_block, e
                );
                true
            }
        }
    }

    async fn scan_outbox(&self) -> bool {
        let claimed_sig = alloy::primitives::keccak256("Claimed(address,uint256,bytes32)");
        let verification_started_sig = alloy::primitives::keccak256("VerificationStarted(uint256)");
        let challenged_sig = alloy::primitives::keccak256("Challenged(uint256,address)");
        let verified_sig = alloy::primitives::keccak256("Verified(uint256)");

        let current_block = match self.route.outbox_provider.get_block_number().await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[{}][Indexer] Failed to get outbox block number: {}", self.route.name, e);
                return true;
            }
        };

        let current_block_data = match self.route.outbox_provider.get_block_by_number(current_block.into()).await {
            Ok(Some(b)) => b,
            _ => return true,
        };
        let now = current_block_data.header.timestamp;

        let state = self.task_store.load();
        let ten_days_blocks = 10 * 24 * 3600 / 12;
        let from_block = state.outbox_last_block.unwrap_or_else(|| current_block.saturating_sub(ten_days_blocks));

        if from_block >= current_block {
            return true;
        }

        let to_block = min(from_block + CHUNK_SIZE, current_block);

        let filter = Filter::new()
            .address(self.route.outbox_address)
            .event_signature(vec![claimed_sig, verification_started_sig, challenged_sig, verified_sig])
            .from_block(from_block)
            .to_block(to_block);

        match self.route.outbox_provider.get_logs(&filter).await {
            Ok(logs) => {
                for log in logs {
                    let block_ts = log.block_timestamp.unwrap_or(0);
                    if block_ts > now.saturating_sub(FINALITY_BUFFER_SECS) {
                        continue;
                    }

                    let topic0 = match log.topics().first() {
                        Some(t) => *t,
                        None => continue,
                    };

                    if topic0 == claimed_sig {
                        self.handle_claimed_event(&log, now).await;
                    } else if topic0 == verification_started_sig {
                        self.handle_verification_started_event(&log).await;
                    } else if topic0 == challenged_sig {
                        self.handle_challenged_event(&log).await;
                    } else if topic0 == verified_sig {
                        self.handle_verified_event(&log).await;
                    }
                }
                self.task_store.update_outbox_block(to_block);
                println!(
                    "[{}][Indexer] Outbox scanned blocks {}-{}",
                    self.route.name, from_block, to_block
                );
                to_block >= current_block
            }
            Err(e) => {
                eprintln!(
                    "[{}][Indexer] Failed to query outbox logs {}-{}: {}",
                    self.route.name, from_block, to_block, e
                );
                true
            }
        }
    }

    async fn handle_snapshot_sent(&self, log: &alloy::rpc::types::Log) {
        let epoch = match self.parse_epoch_from_snapshot_sent(log) {
            Some(e) => e,
            None => return,
        };

        let state = self.task_store.load();
        if state.tasks.iter().any(|t| matches!(t, Task::ExecuteRelay { epoch: e, .. } if *e == epoch)) {
            return;
        }

        let tx_hash = match log.transaction_hash {
            Some(h) => h,
            None => return,
        };

        match self.fetch_l2_to_l1_from_tx(tx_hash, epoch).await {
            Some(task) => {
                println!(
                    "[{}][Indexer] Found SnapshotSent: epoch={}, position={:#x}",
                    self.route.name, epoch, task.2
                );
                self.task_store.add_task(Task::ExecuteRelay {
                    epoch: task.0,
                    execute_after: task.1,
                    position: task.2,
                    l2_sender: task.3,
                    dest_addr: task.4,
                    l2_block: task.5,
                    l1_block: task.6,
                    l2_timestamp: task.7,
                    amount: task.8,
                    data: task.9,
                });
            }
            None => {
                eprintln!("[{}][Indexer] No L2ToL1Tx found in tx {:?}", self.route.name, tx_hash);
            }
        }
    }

    async fn handle_claimed_event(&self, log: &alloy::rpc::types::Log, now: u64) {
        if log.topics().len() < 3 {
            return;
        }

        let claimer = Address::from_slice(&log.topics()[1].0[12..]);
        let epoch = U256::from_be_bytes(log.topics()[2].0).to::<u64>();

        if log.data().data.len() < 32 {
            return;
        }
        let state_root = FixedBytes::<32>::from_slice(&log.data().data[0..32]);

        let block_ts = log.block_timestamp.unwrap_or(0);
        let timestamp_claimed = block_ts as u32;

        let state = self.task_store.load();
        if state.tasks.iter().any(|t| t.epoch() == epoch) {
            return;
        }

        self.claim_store.store(ClaimData {
            epoch,
            state_root,
            claimer,
            timestamp_claimed,
            timestamp_verification: 0,
            blocknumber_verification: 0,
            honest: "None".to_string(),
            challenger: Address::ZERO,
        });

        println!("[{}][Indexer] Claimed event for epoch {} - scheduling VerifyClaim", self.route.name, epoch);

        self.task_store.add_task(Task::VerifyClaim {
            epoch,
            execute_after: now,
        });
    }

    async fn handle_verification_started_event(&self, log: &alloy::rpc::types::Log) {
        if log.topics().len() < 2 {
            return;
        }

        let epoch = U256::from_be_bytes(log.topics()[1].0).to::<u64>();

        let mut state = self.task_store.load();
        state.tasks.retain(|t| !(t.epoch() == epoch && matches!(t, Task::StartVerification { .. })));

        if state.tasks.iter().any(|t| matches!(t, Task::VerifySnapshot { epoch: e, .. } if *e == epoch)) {
            self.task_store.save(&state);
            return;
        }

        let block_ts = log.block_timestamp.unwrap_or(0) as u32;
        let block_num = log.block_number.unwrap_or(0) as u32;

        self.claim_store.update(epoch, |c| {
            c.timestamp_verification = block_ts;
            c.blocknumber_verification = block_num;
        });

        let min_challenge_period = match self.get_min_challenge_period().await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[{}][Indexer] Failed to get minChallengePeriod: {}", self.route.name, e);
                return;
            }
        };

        let execute_after = (block_ts as u64) + min_challenge_period;
        println!(
            "[{}][Indexer] VerificationStarted for epoch {} - scheduled verifySnapshot at {}",
            self.route.name, epoch, execute_after
        );

        state.tasks.push(Task::VerifySnapshot {
            epoch,
            execute_after,
        });
        self.task_store.save(&state);
    }

    async fn handle_challenged_event(&self, log: &alloy::rpc::types::Log) {
        if log.topics().len() < 3 {
            return;
        }

        let epoch = U256::from_be_bytes(log.topics()[1].0).to::<u64>();
        let challenger = Address::from_slice(&log.topics()[2].0[12..]);

        self.claim_store.update(epoch, |c| {
            c.challenger = challenger;
        });

        println!("[{}][Indexer] Challenged event for epoch {} - sending snapshot immediately", self.route.name, epoch);

        if let Err(e) = send_snapshot::execute(&self.route, epoch, &self.claim_store).await {
            eprintln!("[{}][Indexer] Failed to send snapshot for epoch {}: {}", self.route.name, epoch, e);
        }
    }

    async fn handle_verified_event(&self, log: &alloy::rpc::types::Log) {
        let epoch = if log.topics().len() >= 2 {
            U256::from_be_bytes(log.topics()[1].0).to::<u64>()
        } else if log.data().data.len() >= 32 {
            U256::from_be_slice(&log.data().data[0..32]).to::<u64>()
        } else {
            return;
        };

        let claim = self.claim_store.get(epoch);

        let real_state_root = self.get_inbox_snapshot(epoch).await;

        let honest = if claim.state_root == real_state_root {
            "Claimer"
        } else {
            "Challenger"
        };

        self.claim_store.update(epoch, |c| {
            c.honest = honest.to_string();
        });

        println!("[{}][Indexer] Verified event for epoch {} - {} was honest", self.route.name, epoch, honest);

        if let Err(e) = tasks::withdraw_deposit::execute(&self.route, epoch, &self.claim_store).await {
            eprintln!("[{}][Indexer] Failed to withdraw deposit for epoch {}: {}", self.route.name, epoch, e);
        }
    }

    fn parse_epoch_from_snapshot_sent(&self, log: &alloy::rpc::types::Log) -> Option<u64> {
        if log.topics().len() < 2 {
            return None;
        }
        Some(U256::from_be_bytes(log.topics()[1].0).to::<u64>())
    }

    async fn fetch_l2_to_l1_from_tx(
        &self,
        tx_hash: FixedBytes<32>,
        epoch: u64,
    ) -> Option<(u64, u64, U256, Address, Address, u64, u64, u64, U256, Bytes)> {
        let receipt = self.route.inbox_provider.get_transaction_receipt(tx_hash).await.ok()??;

        let l2_to_l1_tx_sig = alloy::primitives::keccak256(
            "L2ToL1Tx(address,address,uint256,uint256,uint256,uint256,uint256,uint256,bytes)"
        );

        for log in receipt.inner.logs() {
            if log.address() != ARB_SYS {
                continue;
            }
            if log.topics().first() != Some(&l2_to_l1_tx_sig) {
                continue;
            }
            if log.topics().len() < 4 {
                continue;
            }

            let caller = Address::from_slice(&log.topics()[1].0[12..]);
            let destination = Address::from_slice(&log.topics()[2].0[12..]);

            let data = &log.data().data;
            if data.len() < 192 {
                continue;
            }

            let position = U256::from_be_slice(&data[0..32]);
            let arb_block_num = U256::from_be_slice(&data[32..64]).to::<u64>();
            let eth_block_num = U256::from_be_slice(&data[64..96]).to::<u64>();
            let timestamp = U256::from_be_slice(&data[96..128]).to::<u64>();
            let callvalue = U256::from_be_slice(&data[128..160]);

            let data_offset = U256::from_be_slice(&data[160..192]).to::<usize>();
            let calldata = if data.len() > data_offset + 32 {
                let data_len = U256::from_be_slice(&data[data_offset..data_offset + 32]).to::<usize>();
                if data.len() >= data_offset + 32 + data_len {
                    Bytes::copy_from_slice(&data[data_offset + 32..data_offset + 32 + data_len])
                } else {
                    Bytes::new()
                }
            } else {
                Bytes::new()
            };

            let block_number = receipt.block_number.unwrap_or(0);
            let block_timestamp = match self.route.inbox_provider.get_block_by_number(block_number.into()).await {
                Ok(Some(block)) => block.header.timestamp,
                _ => 0,
            };

            return Some((
                epoch,
                block_timestamp + RELAY_DELAY,
                position,
                caller,
                destination,
                arb_block_num,
                eth_block_num,
                timestamp,
                callvalue,
                calldata,
            ));
        }
        None
    }

    async fn get_min_challenge_period(&self) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        if self.route.weth_address.is_some() {
            let outbox = IVeaOutboxArbToGnosis::new(self.route.outbox_address, self.route.outbox_provider.clone());
            Ok(outbox.minChallengePeriod().call().await?.to::<u64>())
        } else {
            let outbox = IVeaOutboxArbToEth::new(self.route.outbox_address, self.route.outbox_provider.clone());
            Ok(outbox.minChallengePeriod().call().await?.to::<u64>())
        }
    }

    async fn get_inbox_snapshot(&self, epoch: u64) -> FixedBytes<32> {
        let inbox = IVeaInbox::new(self.route.inbox_address, self.route.inbox_provider.clone());
        inbox.snapshots(U256::from(epoch)).call().await.expect("Failed to get inbox snapshot")
    }
}
