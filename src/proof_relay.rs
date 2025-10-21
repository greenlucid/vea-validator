use alloy::primitives::{Address, FixedBytes, U256, Bytes};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::network::EthereumWallet;
use alloy::rpc::types::TransactionRequest;
use crate::contracts::{IArbSys, IOutbox};
use crate::config::Route;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
const RELAY_DELAY: u64 = 7 * 24 * 3600 + 600;
#[derive(Debug, Clone)]
pub struct L2ToL1MessageData {
    pub ticket_id: FixedBytes<32>,
    pub position: U256,
    pub caller: Address,
    pub destination: Address,
    pub arb_block_num: U256,
    pub eth_block_num: U256,
    pub timestamp: u64,
    pub l2_timestamp: U256,
    pub callvalue: U256,
    pub data: Vec<u8>,
}
pub struct ProofRelay {
    pending: Arc<RwLock<HashMap<u64, L2ToL1MessageData>>>,
    route: Route,
    wallet: EthereumWallet,
}

impl ProofRelay {
    pub fn new(route: Route, wallet: EthereumWallet) -> Self {
        Self {
            pending: Arc::new(RwLock::new(HashMap::new())),
            route,
            wallet,
        }
    }
    pub async fn store_snapshot_sent(&self, epoch: u64, msg_data: L2ToL1MessageData) {
        let mut pending = self.pending.write().await;
        pending.insert(epoch, msg_data);
    }

    async fn execute_relay(&self, epoch: u64, msg_data: L2ToL1MessageData) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        println!("[{}] Executing proof relay for epoch {}", self.route.name, epoch);

        // Get merkle state from ArbSys
        let inbox_provider = ProviderBuilder::new().connect_http(self.route.inbox_rpc.parse()?);
        let arb_sys_addr = Address::from_slice(&[0u8; 19].iter().chain(&[0x64u8]).copied().collect::<Vec<u8>>());
        let arb_sys = IArbSys::new(arb_sys_addr, inbox_provider);
        let merkle_state = arb_sys.sendMerkleTreeState().call().await
            .map_err(|e| format!("Failed to get merkle state: {}", e))?;
        let size = merkle_state.size;

        // Generate proof via NodeInterface
        let node_interface_addr = Address::from_slice(&[0u8; 19].iter().chain(&[0xC8u8]).copied().collect::<Vec<u8>>());
        let proof_bytes = {
            let inbox_provider = ProviderBuilder::new().connect_http(self.route.inbox_rpc.parse()?);
            let mut call_data = vec![0x42, 0x69, 0x6c, 0x6c];
            call_data.extend_from_slice(&size.to_be_bytes::<32>());
            call_data.extend_from_slice(&msg_data.position.to_be_bytes::<32>());
            let result = inbox_provider.call(TransactionRequest::default()
                .to(node_interface_addr)
                .input(Bytes::from(call_data).into()))
                .await
                .map_err(|e| format!("NodeInterface call failed: {}", e))?;
            result
        };

        // Parse proof bytes
        if proof_bytes.len() < 96 {
            return Err(format!("Invalid proof response length: {}", proof_bytes.len()).into());
        }
        let proof_array_offset = U256::from_be_slice(&proof_bytes[64..96]).to::<usize>();
        let proof_start = 96 + proof_array_offset;
        if proof_bytes.len() < proof_start + 32 {
            return Err("Proof data truncated".into());
        }
        let proof_len = U256::from_be_slice(&proof_bytes[proof_start..proof_start + 32]).to::<usize>();
        let mut proof: Vec<FixedBytes<32>> = Vec::new();
        for i in 0..proof_len {
            let offset = proof_start + 32 + (i * 32);
            if proof_bytes.len() < offset + 32 {
                break;
            }
            proof.push(FixedBytes::<32>::from_slice(&proof_bytes[offset..offset + 32]));
        }

        // Execute transaction on outbox
        let outbox_provider = ProviderBuilder::new()
            .wallet(self.wallet.clone())
            .connect_http(self.route.outbox_rpc.parse()?);
        let outbox = IOutbox::new(self.route.outbox_address, outbox_provider);
        let tx = outbox.executeTransaction(
            proof,
            msg_data.position,
            msg_data.caller,
            msg_data.destination,
            msg_data.arb_block_num,
            msg_data.eth_block_num,
            msg_data.l2_timestamp,
            msg_data.callvalue,
            Bytes::from(msg_data.data)
        );
        let receipt = tx.send().await
            .map_err(|e| format!("executeTransaction send failed: {}", e))?
            .get_receipt().await
            .map_err(|e| format!("Receipt fetch failed: {}", e))?;

        if !receipt.status() {
            return Err(format!("executeTransaction failed for epoch {}", epoch).into());
        }

        println!("[{}] Proof relay executed successfully for epoch {}", self.route.name, epoch);
        Ok(())
    }
    pub async fn watch_and_relay(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        loop {
            sleep(Duration::from_secs(60)).await;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            let to_relay: Vec<(u64, L2ToL1MessageData)> = {
                let pending = self.pending.read().await;
                pending.iter()
                    .filter(|(_, msg)| now >= msg.timestamp + RELAY_DELAY)
                    .map(|(epoch, msg)| (*epoch, msg.clone()))
                    .collect()
            };
            for (epoch, msg_data) in to_relay {
                self.execute_relay(epoch, msg_data).await?;
                let mut pending = self.pending.write().await;
                pending.remove(&epoch);
            }
        }
    }
}
