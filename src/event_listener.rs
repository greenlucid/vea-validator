use alloy::primitives::{Address, FixedBytes, U256};
use alloy::providers::{Provider, DynProvider};
use alloy::network::Ethereum;
use alloy::rpc::types::Filter;
use alloy::primitives::keccak256;
use futures_util::StreamExt;
use tokio::time::{sleep, Duration};

#[derive(Debug, Clone)]
pub struct ClaimEvent {
    pub epoch: u64,
    pub state_root: FixedBytes<32>,
    pub claimer: Address,
    pub timestamp_claimed: u32,
}
#[derive(Debug, Clone)]
pub struct SnapshotSentEvent {
    pub epoch: u64,
    pub ticket_id: FixedBytes<32>,
    pub timestamp: u64,
    pub position: U256,
    pub caller: Address,
    pub destination: Address,
    pub arb_block_num: U256,
    pub eth_block_num: U256,
    pub l2_timestamp: U256,
    pub callvalue: U256,
    pub data: Vec<u8>,
}
pub struct EventListener {
    provider: DynProvider<Ethereum>,
    contract_address: Address,
}
impl EventListener {
    pub fn new(provider: DynProvider<Ethereum>, contract_address: Address) -> Self {
        Self {
            provider,
            contract_address,
        }
    }
    pub async fn watch_claims<F, Fut>(&self, handler: F) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        F: Fn(ClaimEvent) -> Fut + Send + 'static + Clone,
        Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send,
    {
        let event_signature = "Claimed(address,uint256,bytes32)";
        let event_hash = keccak256(event_signature.as_bytes());
        loop {
            let filter = Filter::new()
                .address(self.contract_address)
                .event_signature(event_hash);
            match self.provider.watch_logs(&filter).await {
                Ok(subscription) => {
                    let mut stream = subscription.into_stream();
                    while let Some(logs) = stream.next().await {
                        for log in logs {
                            if log.topics().len() >= 3 {
                                let claimer = Address::from_slice(&log.topics()[1].0[12..]);
                                let epoch = U256::from_be_bytes(log.topics()[2].0).to::<u64>();
                                if log.data().data.len() < 32 {
                                    continue;
                                }
                                let state_root = FixedBytes::<32>::from_slice(&log.data().data[0..32]);
                                let block_number = log.block_number.unwrap_or(0);
                                let block = self.provider.get_block_by_number(block_number.into()).await?;
                                let timestamp_claimed = block.unwrap().header.timestamp as u32;
                                let event = ClaimEvent { epoch, state_root, claimer, timestamp_claimed };
                                handler(event).await?;
                            }
                        }
                    }
                    eprintln!("Claim watch stream ended, reconnecting...");
                }
                Err(e) => {
                    eprintln!("Claim watch failed: {}, retrying in 5s...", e);
                }
            }
            sleep(Duration::from_secs(5)).await;
        }
    }
    pub async fn watch_snapshot_sent<F, Fut>(&self, handler: F) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        F: Fn(SnapshotSentEvent) -> Fut + Send + 'static + Clone,
        Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send,
    {
        let event_signature = "SnapshotSent(uint256,bytes32)";
        let event_hash = keccak256(event_signature.as_bytes());
        let arb_sys_address = Address::from_slice(&[0u8; 19].iter().chain(&[0x64u8]).copied().collect::<Vec<u8>>());
        let l2_to_l1_tx_sig = keccak256("L2ToL1Tx(address,address,uint256,uint256,uint256,uint256,uint256,uint256,bytes)".as_bytes());
        loop {
            let filter = Filter::new()
                .address(self.contract_address)
                .event_signature(event_hash);
            match self.provider.watch_logs(&filter).await {
                Ok(subscription) => {
                    let mut stream = subscription.into_stream();
                    while let Some(logs) = stream.next().await {
                        for log in logs {
                            if log.topics().len() >= 2 && log.data().data.len() >= 32 {
                                let epoch = U256::from_be_bytes(log.topics()[1].0).to::<u64>();
                                let ticket_id = FixedBytes::<32>::from_slice(&log.data().data[0..32]);
                                let block_number = log.block_number.unwrap_or(0);
                                let block = self.provider.get_block_by_number(block_number.into()).await?;
                                let timestamp = block.unwrap().header.timestamp;
                                let tx_hash = log.transaction_hash.ok_or("Missing transaction hash")?;
                                let receipt = self.provider.get_transaction_receipt(tx_hash).await?.ok_or("Receipt not found")?;
                                let mut l2_to_l1_data = None;
                                for receipt_log in receipt.inner.logs() {
                                    if receipt_log.address() == arb_sys_address && receipt_log.topics().len() >= 4 && receipt_log.topics()[0] == l2_to_l1_tx_sig {
                                        let position = U256::from_be_bytes(receipt_log.topics()[3].0);
                                        let caller = Address::from_slice(&receipt_log.topics()[1].0[12..]);
                                        let destination = Address::from_slice(&receipt_log.topics()[2].0[12..]);
                                        let data_bytes = &receipt_log.data().data;
                                        if data_bytes.len() >= 224 {
                                            let arb_block_num = U256::from_be_slice(&data_bytes[0..32]);
                                            let eth_block_num = U256::from_be_slice(&data_bytes[32..64]);
                                            let l2_timestamp = U256::from_be_slice(&data_bytes[64..96]);
                                            let callvalue = U256::from_be_slice(&data_bytes[96..128]);
                                            let data_offset = U256::from_be_slice(&data_bytes[128..160]).to::<usize>();
                                            let data_len = U256::from_be_slice(&data_bytes[160..192]).to::<usize>();
                                            let data_start = 192 + data_offset;
                                            let data = if data_start + data_len <= data_bytes.len() {
                                                data_bytes[data_start..data_start + data_len].to_vec()
                                            } else {
                                                vec![]
                                            };
                                            l2_to_l1_data = Some((position, caller, destination, arb_block_num, eth_block_num, l2_timestamp, callvalue, data));
                                            break;
                                        }
                                    }
                                }
                                if let Some((position, caller, destination, arb_block_num, eth_block_num, l2_timestamp, callvalue, data)) = l2_to_l1_data {
                                    let event = SnapshotSentEvent {
                                        epoch,
                                        ticket_id,
                                        timestamp,
                                        position,
                                        caller,
                                        destination,
                                        arb_block_num,
                                        eth_block_num,
                                        l2_timestamp,
                                        callvalue,
                                        data,
                                    };
                                    handler(event).await?;
                                } else {
                                    eprintln!("WARNING: SnapshotSent for epoch {} found but no L2ToL1Tx event", epoch);
                                }
                            }
                        }
                    }
                    eprintln!("SnapshotSent watch stream ended, reconnecting...");
                }
                Err(e) => {
                    eprintln!("SnapshotSent watch failed: {}, retrying in 5s...", e);
                }
            }
            sleep(Duration::from_secs(5)).await;
        }
    }
}
