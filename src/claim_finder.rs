use alloy::primitives::{Address, FixedBytes, U256};
use alloy::providers::{DynProvider, Provider};
use alloy::network::Ethereum;
use alloy::rpc::types::Filter;
use std::path::PathBuf;
use std::cmp::min;
use tokio::time::{sleep, Duration};

use crate::contracts::{IVeaInboxArbToEth, IVeaInboxArbToGnosis, IVeaOutboxArbToEth, IVeaOutboxArbToGnosis, Claim, Party};
use crate::scheduler::{ClaimedData, ScheduleData, ScheduleFile, VerificationTask, VerificationPhase};

const CHUNK_SIZE: u64 = 500;
const FINALITY_BUFFER_SECS: u64 = 15 * 60;
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const RETRY_DELAY: Duration = Duration::from_secs(5);

pub struct ClaimFinder {
    inbox_provider: DynProvider<Ethereum>,
    outbox_provider: DynProvider<Ethereum>,
    inbox_address: Address,
    outbox_address: Address,
    weth_address: Option<Address>,
    wallet_address: Address,
    schedule_path: PathBuf,
    claims_path: PathBuf,
    route_name: &'static str,
}

impl ClaimFinder {
    pub fn new(
        inbox_provider: DynProvider<Ethereum>,
        outbox_provider: DynProvider<Ethereum>,
        inbox_address: Address,
        outbox_address: Address,
        weth_address: Option<Address>,
        wallet_address: Address,
        schedule_path: impl Into<PathBuf>,
        claims_path: impl Into<PathBuf>,
        route_name: &'static str,
    ) -> Self {
        Self {
            inbox_provider,
            outbox_provider,
            inbox_address,
            outbox_address,
            weth_address,
            wallet_address,
            schedule_path: schedule_path.into(),
            claims_path: claims_path.into(),
            route_name,
        }
    }

    pub async fn run(&self) {
        let schedule_file: ScheduleFile<VerificationTask> = ScheduleFile::new(&self.schedule_path);
        let claims_file: ScheduleFile<ClaimedData> = ScheduleFile::new(&self.claims_path);
        let claimed_sig = alloy::primitives::keccak256("Claimed(address,uint256,bytes32)");
        let verification_started_sig = alloy::primitives::keccak256("VerificationStarted(uint256)");
        let challenged_sig = alloy::primitives::keccak256("Challenged(uint256,address)");

        loop {
            let mut schedule = schedule_file.load();

            let current_block = match self.outbox_provider.get_block_number().await {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("[{}][ClaimFinder] Failed to get block number: {}, retrying...", self.route_name, e);
                    sleep(RETRY_DELAY).await;
                    continue;
                }
            };

            let current_block_data = match self.outbox_provider.get_block_by_number(current_block.into()).await {
                Ok(Some(b)) => b,
                _ => {
                    sleep(RETRY_DELAY).await;
                    continue;
                }
            };
            let now = current_block_data.header.timestamp;

            let from_block = schedule.last_checked_block.unwrap_or_else(|| {
                let ten_days_blocks = 10 * 24 * 3600 / 12;
                current_block.saturating_sub(ten_days_blocks)
            });

            if from_block >= current_block {
                println!("[{}][ClaimFinder] Caught up to block {}, waiting...", self.route_name, current_block);
                sleep(POLL_INTERVAL).await;
                continue;
            }

            let to_block = min(from_block + CHUNK_SIZE, current_block);

            let filter = Filter::new()
                .address(self.outbox_address)
                .event_signature(vec![claimed_sig, verification_started_sig, challenged_sig])
                .from_block(from_block)
                .to_block(to_block);

            match self.outbox_provider.get_logs(&filter).await {
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
                            self.handle_claimed_event(&log, &mut schedule, &claims_file, now).await;
                        } else if topic0 == verification_started_sig {
                            self.handle_verification_started_event(&log, &mut schedule, &claims_file).await;
                        } else if topic0 == challenged_sig {
                            self.handle_challenged_event(&log, &claims_file).await;
                        }
                    }
                    schedule.last_checked_block = Some(to_block);
                    schedule_file.save(&schedule);
                    println!(
                        "[{}][ClaimFinder] Scanned blocks {}-{}, {} pending tasks",
                        self.route_name, from_block, to_block, schedule.pending.len()
                    );
                }
                Err(e) => {
                    eprintln!(
                        "[{}][ClaimFinder] Failed to query logs {}-{}: {}, retrying...",
                        self.route_name, from_block, to_block, e
                    );
                    sleep(RETRY_DELAY).await;
                }
            }
        }
    }

    async fn handle_claimed_event(
        &self,
        log: &alloy::rpc::types::Log,
        schedule: &mut ScheduleData<VerificationTask>,
        claims_file: &ScheduleFile<ClaimedData>,
        now: u64,
    ) {
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

        let mut claims = claims_file.load();
        if !claims.pending.iter().any(|c| c.epoch == epoch) {
            claims.pending.push(ClaimedData {
                epoch,
                state_root,
                claimer,
                timestamp_claimed,
            });
            claims_file.save(&claims);
        }

        if schedule.pending.iter().any(|t| t.epoch == epoch) {
            return;
        }

        let correct_root = match self.get_correct_state_root(epoch).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[{}][ClaimFinder] Failed to get snapshot for epoch {}: {}", self.route_name, epoch, e);
                return;
            }
        };

        if state_root != correct_root {
            println!("[{}][ClaimFinder] INVALID claim detected for epoch {} - challenging!", self.route_name, epoch);
            let claim = Claim {
                stateRoot: state_root,
                claimer,
                timestampClaimed: timestamp_claimed,
                timestampVerification: 0,
                blocknumberVerification: 0,
                honest: Party::None,
                challenger: Address::ZERO,
            };
            if let Err(e) = self.challenge_claim(epoch, claim).await {
                eprintln!("[{}][ClaimFinder] Challenge failed for epoch {}: {}", self.route_name, epoch, e);
            }
            return;
        }

        let (seq_delay, epoch_period) = match self.get_timing_params().await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[{}][ClaimFinder] Failed to get timing params: {}", self.route_name, e);
                return;
            }
        };

        let execute_after = (timestamp_claimed as u64) + seq_delay + epoch_period;
        if execute_after <= now {
            println!("[{}][ClaimFinder] Valid claim for epoch {} ready for startVerification", self.route_name, epoch);
        } else {
            println!("[{}][ClaimFinder] Valid claim for epoch {} scheduled for startVerification at {}", self.route_name, epoch, execute_after);
        }

        schedule.pending.push(VerificationTask {
            epoch,
            execute_after,
            phase: VerificationPhase::StartVerification,
            state_root,
            claimer,
            timestamp_claimed,
            timestamp_verification: 0,
            blocknumber_verification: 0,
        });
    }

    async fn handle_verification_started_event(
        &self,
        log: &alloy::rpc::types::Log,
        schedule: &mut ScheduleData<VerificationTask>,
        claims_file: &ScheduleFile<ClaimedData>,
    ) {
        if log.topics().len() < 2 {
            return;
        }

        let epoch = U256::from_be_bytes(log.topics()[1].0).to::<u64>();

        schedule.pending.retain(|t| !(t.epoch == epoch && matches!(t.phase, VerificationPhase::StartVerification)));

        if schedule.pending.iter().any(|t| t.epoch == epoch && matches!(t.phase, VerificationPhase::VerifySnapshot)) {
            return;
        }

        let block_ts = log.block_timestamp.unwrap_or(0) as u32;
        let block_num = log.block_number.unwrap_or(0) as u32;

        let min_challenge_period = match self.get_min_challenge_period().await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[{}][ClaimFinder] Failed to get minChallengePeriod: {}", self.route_name, e);
                return;
            }
        };

        let claims = claims_file.load();
        let claimed_data = match claims.pending.iter().find(|c| c.epoch == epoch) {
            Some(c) => c,
            None => {
                panic!("[{}][ClaimFinder] FATAL: No ClaimedData found for epoch {} during VerificationStarted", self.route_name, epoch);
            }
        };

        let execute_after = (block_ts as u64) + min_challenge_period;
        println!(
            "[{}][ClaimFinder] VerificationStarted for epoch {} - scheduled verifySnapshot at {}",
            self.route_name, epoch, execute_after
        );

        schedule.pending.push(VerificationTask {
            epoch,
            execute_after,
            phase: VerificationPhase::VerifySnapshot,
            state_root: claimed_data.state_root,
            claimer: claimed_data.claimer,
            timestamp_claimed: claimed_data.timestamp_claimed,
            timestamp_verification: block_ts,
            blocknumber_verification: block_num,
        });
    }

    async fn handle_challenged_event(&self, log: &alloy::rpc::types::Log, claims_file: &ScheduleFile<ClaimedData>) {
        if log.topics().len() < 3 {
            return;
        }

        let epoch = U256::from_be_bytes(log.topics()[1].0).to::<u64>();
        let challenger = Address::from_slice(&log.topics()[2].0[12..]);

        let claims = claims_file.load();
        let claimed_data = match claims.pending.iter().find(|c| c.epoch == epoch) {
            Some(c) => c,
            None => {
                panic!("[{}][ClaimFinder] FATAL: No ClaimedData found for challenged epoch {}", self.route_name, epoch);
            }
        };

        let claim = Claim {
            stateRoot: claimed_data.state_root,
            claimer: claimed_data.claimer,
            timestampClaimed: claimed_data.timestamp_claimed,
            timestampVerification: 0,
            blocknumberVerification: 0,
            honest: Party::None,
            challenger,
        };

        println!("[{}][ClaimFinder] Challenged event for epoch {} - calling sendSnapshot", self.route_name, epoch);

        if self.weth_address.is_some() {
            let inbox = IVeaInboxArbToGnosis::new(self.inbox_address, self.inbox_provider.clone());
            let gas_limit = U256::from(500000);
            match inbox.sendSnapshot(U256::from(epoch), gas_limit, claim).send().await {
                Ok(pending) => {
                    match pending.get_receipt().await {
                        Ok(receipt) if receipt.status() => {
                            println!("[{}][ClaimFinder] sendSnapshot succeeded for epoch {}", self.route_name, epoch);
                        }
                        Ok(_) => {
                            panic!("[{}][ClaimFinder] FATAL: sendSnapshot reverted for epoch {}", self.route_name, epoch);
                        }
                        Err(e) => {
                            panic!("[{}][ClaimFinder] FATAL: sendSnapshot failed for epoch {}: {}", self.route_name, epoch, e);
                        }
                    }
                }
                Err(e) => {
                    panic!("[{}][ClaimFinder] FATAL: sendSnapshot failed for epoch {}: {}", self.route_name, epoch, e);
                }
            }
        } else {
            let inbox = IVeaInboxArbToEth::new(self.inbox_address, self.inbox_provider.clone());
            match inbox.sendSnapshot(U256::from(epoch), claim).send().await {
                Ok(pending) => {
                    match pending.get_receipt().await {
                        Ok(receipt) if receipt.status() => {
                            println!("[{}][ClaimFinder] sendSnapshot succeeded for epoch {}", self.route_name, epoch);
                        }
                        Ok(_) => {
                            panic!("[{}][ClaimFinder] FATAL: sendSnapshot reverted for epoch {}", self.route_name, epoch);
                        }
                        Err(e) => {
                            panic!("[{}][ClaimFinder] FATAL: sendSnapshot failed for epoch {}: {}", self.route_name, epoch, e);
                        }
                    }
                }
                Err(e) => {
                    panic!("[{}][ClaimFinder] FATAL: sendSnapshot failed for epoch {}: {}", self.route_name, epoch, e);
                }
            }
        }
    }

    async fn get_correct_state_root(&self, epoch: u64) -> Result<FixedBytes<32>, Box<dyn std::error::Error + Send + Sync>> {
        let inbox = IVeaInboxArbToEth::new(self.inbox_address, self.inbox_provider.clone());
        Ok(inbox.snapshots(U256::from(epoch)).call().await?)
    }

    async fn get_timing_params(&self) -> Result<(u64, u64), Box<dyn std::error::Error + Send + Sync>> {
        if self.weth_address.is_some() {
            let outbox = IVeaOutboxArbToGnosis::new(self.outbox_address, self.outbox_provider.clone());
            let seq_delay = outbox.sequencerDelayLimit().call().await?.to::<u64>();
            let epoch_period = outbox.epochPeriod().call().await?.to::<u64>();
            Ok((seq_delay, epoch_period))
        } else {
            let outbox = IVeaOutboxArbToEth::new(self.outbox_address, self.outbox_provider.clone());
            let seq_delay = outbox.sequencerDelayLimit().call().await?.to::<u64>();
            let epoch_period = outbox.epochPeriod().call().await?.to::<u64>();
            Ok((seq_delay, epoch_period))
        }
    }

    async fn get_min_challenge_period(&self) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        if self.weth_address.is_some() {
            let outbox = IVeaOutboxArbToGnosis::new(self.outbox_address, self.outbox_provider.clone());
            Ok(outbox.minChallengePeriod().call().await?.to::<u64>())
        } else {
            let outbox = IVeaOutboxArbToEth::new(self.outbox_address, self.outbox_provider.clone());
            Ok(outbox.minChallengePeriod().call().await?.to::<u64>())
        }
    }

    async fn challenge_claim(&self, epoch: u64, claim: Claim) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.weth_address.is_some() {
            let outbox = IVeaOutboxArbToGnosis::new(self.outbox_address, self.outbox_provider.clone());
            let tx = outbox.challenge(U256::from(epoch), claim);
            let pending = match tx.send().await {
                Ok(p) => p,
                Err(e) => {
                    let err_msg = e.to_string();
                    if err_msg.contains("Invalid claim") {
                        println!("[{}][ClaimFinder] Claim already challenged - bridge is safe", self.route_name);
                        return Ok(());
                    }
                    panic!("[{}][ClaimFinder] FATAL: Unexpected error challenging epoch {}: {}", self.route_name, epoch, e);
                }
            };
            let receipt = pending.get_receipt().await?;
            if !receipt.status() {
                panic!("[{}][ClaimFinder] FATAL: Challenge tx reverted for epoch {}", self.route_name, epoch);
            }
            println!("[{}][ClaimFinder] Successfully challenged epoch {}", self.route_name, epoch);
        } else {
            let outbox = IVeaOutboxArbToEth::new(self.outbox_address, self.outbox_provider.clone());
            let deposit = outbox.deposit().call().await?;
            let tx = outbox.challenge(U256::from(epoch), claim, self.wallet_address).value(deposit);
            let pending = match tx.send().await {
                Ok(p) => p,
                Err(e) => {
                    let err_msg = e.to_string();
                    if err_msg.contains("Invalid claim") {
                        println!("[{}][ClaimFinder] Claim already challenged - bridge is safe", self.route_name);
                        return Ok(());
                    }
                    panic!("[{}][ClaimFinder] FATAL: Unexpected error challenging epoch {}: {}", self.route_name, epoch, e);
                }
            };
            let receipt = pending.get_receipt().await?;
            if !receipt.status() {
                panic!("[{}][ClaimFinder] FATAL: Challenge tx reverted for epoch {}", self.route_name, epoch);
            }
            println!("[{}][ClaimFinder] Successfully challenged epoch {}", self.route_name, epoch);
        }
        Ok(())
    }
}
