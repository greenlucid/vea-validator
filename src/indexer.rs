use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use alloy::providers::Provider;
use alloy::rpc::types::Filter;
use std::cmp::min;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::time::{sleep, Duration};

use crate::config::Route;
use crate::contracts::{IVeaInbox, IVeaOutboxArbToEth, IVeaOutboxArbToGnosis};
use crate::tasks::{Task, TaskKind, TaskStore, ClaimStore, ClaimData};

use alloy::network::Ethereum;
use alloy::providers::DynProvider;

enum ScanTarget { Inbox, Outbox }

const CHUNK_SIZE: u64 = 2000;
const FINALITY_BUFFER_SECS: u64 = 15 * 60;
const CATCHUP_SLEEP: Duration = Duration::from_secs(1);
const IDLE_SLEEP: Duration = Duration::from_secs(5 * 60);
const RELAY_DELAY: u64 = 7 * 24 * 3600 + 3600;
const SYNC_LOOKBACK_SECS: u64 = 18 * 3600;
const ARB_SYS: Address = Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x64]);

async fn get_log_timestamp(log: &alloy::rpc::types::Log, provider: &DynProvider<Ethereum>) -> u64 {
    if let Some(ts) = log.block_timestamp {
        return ts;
    }
    let block_num = log.block_number.expect("Log missing block_number");
    let block = provider.get_block_by_number(block_num.into()).await
        .expect("Failed to fetch block for timestamp")
        .expect("Block not found");
    block.header.timestamp
}

async fn find_block_by_timestamp(provider: &DynProvider<Ethereum>, target_ts: u64) -> u64 {
    let latest = provider.get_block_number().await.expect("Failed to get latest block number");
    let latest_block = provider.get_block_by_number(latest.into()).await
        .expect("Failed to get latest block")
        .expect("Latest block not found");

    if target_ts >= latest_block.header.timestamp {
        return latest;
    }

    let mut lo = 0u64;
    let mut hi = latest;

    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let block = provider.get_block_by_number(mid.into()).await
            .expect("Failed to get block during binary search")
            .expect("Block not found during binary search");
        if block.header.timestamp < target_ts {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

pub struct EventIndexer {
    route: Route,
    wallet_address: Address,
    task_store: Arc<Mutex<TaskStore>>,
    claim_store: Arc<Mutex<ClaimStore>>,
    inbox_catchup: (AtomicU64, AtomicU64),
    outbox_catchup: (AtomicU64, AtomicU64),
}

impl EventIndexer {
    pub fn new(
        route: Route,
        wallet_address: Address,
        task_store: Arc<Mutex<TaskStore>>,
        claim_store: Arc<Mutex<ClaimStore>>,
    ) -> Self {
        Self {
            route,
            wallet_address,
            task_store,
            claim_store,
            inbox_catchup: (AtomicU64::new(0), AtomicU64::new(0)),
            outbox_catchup: (AtomicU64::new(0), AtomicU64::new(0)),
        }
    }

    pub async fn initialize(&self) {
        self.task_store.lock().unwrap().set_on_sync(false);
        let state = self.task_store.lock().unwrap().load();

        let inbox_now = self.route.inbox_provider.get_block_by_number(Default::default()).await
            .expect("Failed to get inbox block")
            .expect("Inbox block not found")
            .header.timestamp;

        let needs_init = match (state.inbox_last_block, state.outbox_last_block) {
            (None, _) | (_, None) => true,
            (Some(inbox_b), Some(outbox_b)) => {
                let inbox_ts = self.route.inbox_provider.get_block_by_number(inbox_b.into()).await
                    .expect("Failed to get inbox last block")
                    .expect("Inbox last block not found")
                    .header.timestamp;
                let outbox_ts = self.route.outbox_provider.get_block_by_number(outbox_b.into()).await
                    .expect("Failed to get outbox last block")
                    .expect("Outbox last block not found")
                    .header.timestamp;
                inbox_ts < inbox_now.saturating_sub(SYNC_LOOKBACK_SECS)
                    || outbox_ts < inbox_now.saturating_sub(SYNC_LOOKBACK_SECS)
            }
        };

        if needs_init {
            let indexing_since = inbox_now.saturating_sub(SYNC_LOOKBACK_SECS);
            let inbox_start = find_block_by_timestamp(&self.route.inbox_provider, indexing_since).await;
            let outbox_start = find_block_by_timestamp(&self.route.outbox_provider, indexing_since).await;
            self.task_store.lock().unwrap().initialize_sync(indexing_since, inbox_start, outbox_start);
            println!("[{}][Indexer] Initialized sync: indexing_since={}, inbox_start={}, outbox_start={}",
                self.route.name, indexing_since, inbox_start, outbox_start);
        }
    }

    pub async fn run(&self) {
        loop {
            let done = self.scan_once().await;
            if done {
                if !self.task_store.lock().unwrap().is_on_sync() {
                    println!("[{}][Indexer] Sync complete", self.route.name);
                    self.task_store.lock().unwrap().set_on_sync(true);
                }
                sleep(IDLE_SLEEP).await;
            } else {
                sleep(CATCHUP_SLEEP).await;
            }
        }
    }

    pub async fn scan_once(&self) -> bool {
        let inbox_done = self.scan_chain(ScanTarget::Inbox).await;
        let outbox_done = self.scan_chain(ScanTarget::Outbox).await;
        inbox_done && outbox_done
    }

    async fn scan_chain(&self, target: ScanTarget) -> bool {
        use ScanTarget::*;

        let (provider, address, label, catchup) = match target {
            Inbox => (&self.route.inbox_provider, self.route.inbox_address, "Inbox", &self.inbox_catchup),
            Outbox => (&self.route.outbox_provider, self.route.outbox_address, "Outbox", &self.outbox_catchup),
        };

        let current_block = match provider.get_block_number().await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[{}][Indexer] Failed to get {} block number: {}", self.route.name, label, e);
                return false;
            }
        };

        let current_block_data = provider.get_block_by_number(current_block.into()).await
            .expect("Failed to get block data")
            .expect("Block not found");
        let now = current_block_data.header.timestamp;

        let state = self.task_store.lock().unwrap().load();
        let from_block = match target {
            Inbox => state.inbox_last_block.expect("inbox_last_block not set"),
            Outbox => state.outbox_last_block.expect("outbox_last_block not set"),
        };

        let (catchup_start, catchup_target) = catchup;
        let mut target_block = catchup_target.load(Ordering::Relaxed);

        if target_block == 0 || from_block >= target_block {
            let target_ts = now.saturating_sub(FINALITY_BUFFER_SECS);
            target_block = find_block_by_timestamp(provider, target_ts).await;
            catchup_target.store(target_block, Ordering::Relaxed);
        }

        if from_block >= target_block {
            catchup_start.store(0, Ordering::Relaxed);
            catchup_target.store(0, Ordering::Relaxed);
            return true;
        }

        if catchup_start.load(Ordering::Relaxed) == 0 {
            catchup_start.store(from_block, Ordering::Relaxed);
        }

        let to_block = min(from_block + CHUNK_SIZE, target_block);

        let event_sigs: Vec<FixedBytes<32>> = match target {
            Inbox => vec![alloy::primitives::keccak256("SnapshotSent(uint256,bytes32)")],
            Outbox => vec![
                alloy::primitives::keccak256("Claimed(address,uint256,bytes32)"),
                alloy::primitives::keccak256("VerificationStarted(uint256)"),
                alloy::primitives::keccak256("Challenged(uint256,address)"),
                alloy::primitives::keccak256("Verified(uint256)"),
            ],
        };

        let filter = Filter::new()
            .address(address)
            .event_signature(event_sigs)
            .from_block(from_block)
            .to_block(to_block);

        match provider.get_logs(&filter).await {
            Ok(logs) => {
                for log in logs {
                    let block_ts = get_log_timestamp(&log, provider).await;
                    if block_ts > now.saturating_sub(FINALITY_BUFFER_SECS) {
                        continue;
                    }
                    match target {
                        Inbox => self.handle_snapshot_sent(&log).await,
                        Outbox => self.dispatch_outbox_event(&log).await,
                    }
                }

                match target {
                    Inbox => self.task_store.lock().unwrap().update_inbox_block(to_block),
                    Outbox => self.task_store.lock().unwrap().update_outbox_block(to_block),
                }

                let is_done = to_block >= target_block;

                let start = catchup_start.load(Ordering::Relaxed);
                if start != 0 && !is_done {
                    let total = target_block.saturating_sub(start);
                    let done = to_block.saturating_sub(start);
                    let pct = if total > 0 { (done * 100) / total } else { 100 };
                    println!("[{}][Indexer] {} {}-{} ({}%)", self.route.name, label, from_block, to_block, pct);
                }
                if is_done {
                    catchup_start.store(0, Ordering::Relaxed);
                    catchup_target.store(0, Ordering::Relaxed);
                }
                is_done
            }
            Err(e) => {
                eprintln!("[{}][Indexer] Failed to query {} logs {}-{}: {}", self.route.name, label, from_block, to_block, e);
                false
            }
        }
    }

    async fn dispatch_outbox_event(&self, log: &alloy::rpc::types::Log) {
        let topic0 = match log.topics().first() {
            Some(t) => *t,
            None => return,
        };

        if topic0 == alloy::primitives::keccak256("Claimed(address,uint256,bytes32)") {
            self.handle_claimed_event(log).await;
        } else if topic0 == alloy::primitives::keccak256("VerificationStarted(uint256)") {
            self.handle_verification_started_event(log).await;
        } else if topic0 == alloy::primitives::keccak256("Challenged(uint256,address)") {
            self.handle_challenged_event(log).await;
        } else if topic0 == alloy::primitives::keccak256("Verified(uint256)") {
            self.handle_verified_event(log).await;
        }
    }

    async fn handle_snapshot_sent(&self, log: &alloy::rpc::types::Log) {
        let epoch = match self.parse_epoch_from_snapshot_sent(log) {
            Some(e) => e,
            None => return,
        };

        let state = self.task_store.lock().unwrap().load();
        if state.tasks.iter().any(|t| t.epoch == epoch && matches!(t.kind, TaskKind::ExecuteRelay { .. })) {
            return;
        }

        let tx_hash = match log.transaction_hash {
            Some(h) => h,
            None => return,
        };

        let tx = self.route.inbox_provider.get_transaction_by_hash(tx_hash).await
            .expect("Failed to get transaction")
            .expect("Transaction not found");
        if tx.inner.signer() != self.wallet_address {
            println!("[{}][Indexer] SnapshotSent for epoch {} not from validator, skipping", self.route.name, epoch);
            return;
        }

        match self.fetch_l2_to_l1_from_tx(tx_hash, epoch).await {
            Some(task) => {
                println!(
                    "[{}][Indexer] Found SnapshotSent: epoch={}, position={:#x}",
                    self.route.name, epoch, task.2
                );
                self.task_store.lock().unwrap().add_task(Task {
                    epoch: task.0,
                    execute_after: task.1,
                    kind: TaskKind::ExecuteRelay {
                        position: task.2,
                        l2_sender: task.3,
                        dest_addr: task.4,
                        l2_block: task.5,
                        l1_block: task.6,
                        l2_timestamp: task.7,
                        amount: task.8,
                        data: task.9,
                    },
                });
            }
            None => {
                eprintln!("[{}][Indexer] No L2ToL1Tx found in tx {:?}", self.route.name, tx_hash);
            }
        }
    }

    async fn handle_claimed_event(&self, log: &alloy::rpc::types::Log) {
        if log.topics().len() < 3 {
            return;
        }

        let claimer = Address::from_slice(&log.topics()[1].0[12..]);
        let epoch = U256::from_be_bytes(log.topics()[2].0).to::<u64>();
        println!("[{}][Indexer] Claimed event for epoch {} at block {}", self.route.name, epoch, log.block_number.unwrap_or(0));

        if log.data().data.len() < 32 {
            return;
        }
        let state_root = FixedBytes::<32>::from_slice(&log.data().data[0..32]);

        let block_ts = get_log_timestamp(log, &self.route.outbox_provider).await;
        let timestamp_claimed = block_ts as u32;

        let state = self.task_store.lock().unwrap().load();
        if state.tasks.iter().any(|t| t.epoch == epoch) {
            return;
        }

        self.claim_store.lock().unwrap().store(ClaimData {
            epoch,
            state_root,
            claimer,
            timestamp_claimed,
            timestamp_verification: 0,
            blocknumber_verification: 0,
            honest: "None".to_string(),
            challenger: Address::ZERO,
        });

        self.task_store.lock().unwrap().add_task(Task {
            epoch,
            execute_after: block_ts,
            kind: TaskKind::ValidateClaim,
        });
    }

    async fn handle_verification_started_event(&self, log: &alloy::rpc::types::Log) {
        if log.topics().len() < 2 {
            return;
        }

        let epoch = U256::from_be_bytes(log.topics()[1].0).to::<u64>();
        println!("[{}][Indexer] VerificationStarted event for epoch {} at block {}", self.route.name, epoch, log.block_number.unwrap_or(0));

        if !self.claim_store.lock().unwrap().exists(epoch) {
            let block_ts = get_log_timestamp(log, &self.route.outbox_provider).await;
            let state = self.task_store.lock().unwrap().load();
            let grace_end = state.indexing_since.unwrap_or(0) + SYNC_LOOKBACK_SECS;

            if block_ts < grace_end {
                println!("[{}][Indexer] Dropping VerificationStarted for epoch {} - claim outside sync window", self.route.name, epoch);
                return;
            }
            panic!("[{}] VerificationStarted for epoch {} but claim not found - this is a bug", self.route.name, epoch);
        }

        let block_ts = get_log_timestamp(log, &self.route.outbox_provider).await as u32;
        let block_num = log.block_number.expect("Log missing block_number") as u32;

        self.claim_store.lock().unwrap().update(epoch, |c| {
            c.timestamp_verification = block_ts;
            c.blocknumber_verification = block_num;
        });

        let min_challenge_period = self.get_min_challenge_period().await
            .expect("Failed to get minChallengePeriod");

        let execute_after = (block_ts as u64) + min_challenge_period;

        self.task_store.lock().unwrap().add_task(Task {
            epoch,
            execute_after,
            kind: TaskKind::VerifySnapshot,
        });
    }

    async fn handle_challenged_event(&self, log: &alloy::rpc::types::Log) {
        if log.topics().len() < 3 {
            return;
        }

        let epoch = U256::from_be_bytes(log.topics()[1].0).to::<u64>();
        let challenger = Address::from_slice(&log.topics()[2].0[12..]);
        println!("[{}][Indexer] Challenged event for epoch {} at block {}", self.route.name, epoch, log.block_number.unwrap_or(0));

        if !self.claim_store.lock().unwrap().exists(epoch) {
            let block_ts = get_log_timestamp(log, &self.route.outbox_provider).await;
            let state = self.task_store.lock().unwrap().load();
            let grace_end = state.indexing_since.unwrap_or(0) + SYNC_LOOKBACK_SECS;

            if block_ts < grace_end {
                println!("[{}][Indexer] Dropping Challenged for epoch {} - claim outside sync window", self.route.name, epoch);
                return;
            }
            panic!("[{}] Challenged for epoch {} but claim not found - this is a bug", self.route.name, epoch);
        }

        self.claim_store.lock().unwrap().update(epoch, |c| {
            c.challenger = challenger;
        });

        let block_ts = get_log_timestamp(log, &self.route.outbox_provider).await;
        self.task_store.lock().unwrap().add_task(Task {
            epoch,
            execute_after: block_ts,
            kind: TaskKind::SendSnapshot,
        });
    }

    async fn handle_verified_event(&self, log: &alloy::rpc::types::Log) {
        let epoch = if log.topics().len() >= 2 {
            U256::from_be_bytes(log.topics()[1].0).to::<u64>()
        } else if log.data().data.len() >= 32 {
            U256::from_be_slice(&log.data().data[0..32]).to::<u64>()
        } else {
            return;
        };
        println!("[{}][Indexer] Verified event for epoch {} at block {}", self.route.name, epoch, log.block_number.unwrap_or(0));

        let block_ts = get_log_timestamp(log, &self.route.outbox_provider).await;

        if !self.claim_store.lock().unwrap().exists(epoch) {
            let state = self.task_store.lock().unwrap().load();
            let grace_end = state.indexing_since.unwrap_or(0) + SYNC_LOOKBACK_SECS;

            if block_ts < grace_end {
                println!("[{}][Indexer] Dropping Verified for epoch {} - claim outside sync window", self.route.name, epoch);
                return;
            }
            panic!("[{}] Verified for epoch {} but claim not found - this is a bug", self.route.name, epoch);
        }

        let claim = self.claim_store.lock().unwrap().get(epoch);

        let real_state_root = self.get_inbox_snapshot(epoch).await;

        let honest = if claim.state_root == real_state_root {
            "Claimer"
        } else {
            "Challenger"
        };

        self.claim_store.lock().unwrap().update(epoch, |c| {
            c.honest = honest.to_string();
        });

        self.task_store.lock().unwrap().add_task(Task {
            epoch,
            execute_after: block_ts,
            kind: TaskKind::WithdrawDeposit,
        });
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
        let receipt = self.route.inbox_provider.get_transaction_receipt(tx_hash).await
            .expect("Failed to get transaction receipt")
            .expect("Transaction receipt not found");

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

            let block_number = receipt.block_number.expect("Receipt missing block_number");
            let block_timestamp = self.route.inbox_provider.get_block_by_number(block_number.into()).await
                .expect("Failed to get block by number")
                .expect("Block not found")
                .header.timestamp;

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
