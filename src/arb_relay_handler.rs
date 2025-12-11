use alloy::primitives::{Address, FixedBytes, U256};
use alloy::providers::{DynProvider, Provider};
use alloy::network::Ethereum;
use std::path::PathBuf;
use tokio::time::{sleep, Duration};

use crate::contracts::{IArbSys, INodeInterface, IOutbox};
use crate::scheduler::{ArbToL1Task, ScheduleFile};

const POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);
const ARB_SYS: Address = Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x64]);
const NODE_INTERFACE: Address = Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xC8]);

pub struct ArbRelayHandler {
    arb_provider: DynProvider<Ethereum>,
    eth_provider: DynProvider<Ethereum>,
    outbox_address: Address,
    schedule_path: PathBuf,
}

impl ArbRelayHandler {
    pub fn new(
        arb_provider: DynProvider<Ethereum>,
        eth_provider: DynProvider<Ethereum>,
        outbox_address: Address,
        schedule_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            arb_provider,
            eth_provider,
            outbox_address,
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
        let schedule_file: ScheduleFile<ArbToL1Task> = ScheduleFile::new(&self.schedule_path);
        let mut schedule = schedule_file.load();

        let now = match self.eth_provider.get_block_by_number(Default::default()).await {
            Ok(Some(block)) => block.header.timestamp,
            _ => return,
        };

        let ready: Vec<ArbToL1Task> = schedule
            .pending
            .iter()
            .filter(|t| now >= t.execute_after)
            .cloned()
            .collect();

        if ready.is_empty() {
            return;
        }

        println!("[ArbRelayHandler] Processing {} ready tasks", ready.len());

        let outbox = IOutbox::new(self.outbox_address, self.eth_provider.clone());

        for task in ready {
            match outbox.isSpent(task.position).call().await {
                Ok(is_spent) if is_spent => {
                    println!(
                        "[ArbRelayHandler] Epoch {} already relayed (position {:#x})",
                        task.epoch, task.position
                    );
                    schedule.pending.retain(|t| t.epoch != task.epoch);
                }
                Ok(_) => {
                    println!(
                        "[ArbRelayHandler] Executing relay for epoch {} (position {:#x})\n  l2_sender: {:?}\n  dest_addr: {:?}\n  data len: {}",
                        task.epoch, task.position, task.l2_sender, task.dest_addr, task.data.len()
                    );

                    let proof = match self.fetch_outbox_proof(task.position).await {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!(
                                "[ArbRelayHandler] Failed to fetch proof for epoch {}: {}",
                                task.epoch, e
                            );
                            continue;
                        }
                    };

                    match outbox
                        .executeTransaction(
                            proof,
                            task.position,
                            task.l2_sender,
                            task.dest_addr,
                            U256::from(task.l2_block),
                            U256::from(task.l1_block),
                            U256::from(task.l2_timestamp),
                            task.amount,
                            task.data.clone(),
                        )
                        .send()
                        .await
                    {
                        Ok(pending) => {
                            match pending.get_receipt().await {
                                Ok(receipt) => {
                                    println!(
                                        "[ArbRelayHandler] Epoch {} relayed successfully! tx: {:?}",
                                        task.epoch, receipt.transaction_hash
                                    );
                                    schedule.pending.retain(|t| t.epoch != task.epoch);
                                }
                                Err(e) => {
                                    eprintln!(
                                        "[ArbRelayHandler] Epoch {} tx failed to confirm: {}",
                                        task.epoch, e
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "[ArbRelayHandler] Failed to execute relay for epoch {}: {}",
                                task.epoch, e
                            );
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[ArbRelayHandler] Failed to check isSpent for epoch {}: {}",
                        task.epoch, e
                    );
                }
            }
        }

        schedule_file.save(&schedule);
    }

    async fn fetch_outbox_proof(
        &self,
        position: U256,
    ) -> Result<Vec<FixedBytes<32>>, Box<dyn std::error::Error + Send + Sync>> {
        let arb_sys = IArbSys::new(ARB_SYS, self.arb_provider.clone());
        let state = arb_sys.sendMerkleTreeState().call().await?;
        let size = state.size.to::<u64>();

        let node_interface = INodeInterface::new(NODE_INTERFACE, self.arb_provider.clone());
        let leaf = position.to::<u64>();
        let result = node_interface.constructOutboxProof(size, leaf).call().await?;

        Ok(result.proof)
    }
}
