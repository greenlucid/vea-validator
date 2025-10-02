use alloy::primitives::{Address, FixedBytes, U256};
use alloy::providers::Provider;
use alloy::rpc::types::Filter;
use alloy::primitives::keccak256;
use futures_util::StreamExt;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct SnapshotEvent {
    pub epoch: u64,
    pub state_root: FixedBytes<32>,
    pub count: u64,
}

#[derive(Debug, Clone)]
pub struct ClaimEvent {
    pub epoch: u64,
    pub state_root: FixedBytes<32>,
    pub claimer: Address,
}

pub struct EventListener<P: Provider> {
    provider: Arc<P>,
    contract_address: Address,
}

impl<P: Provider> EventListener<P> {
    pub fn new(provider: Arc<P>, contract_address: Address) -> Self {
        Self {
            provider,
            contract_address,
        }
    }

    pub async fn watch_snapshots<F, Fut>(&self, handler: F) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        F: Fn(SnapshotEvent) -> Fut + Send + 'static + Clone,
        Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send,
    {
        let event_signature = "SnapshotSaved(bytes32,uint256,uint64)";
        let event_hash = keccak256(event_signature.as_bytes());

        let filter = Filter::new()
            .address(self.contract_address)
            .event_signature(event_hash);
        let subscription = self.provider.watch_logs(&filter).await?;
        let mut stream = subscription.into_stream();

        while let Some(logs) = stream.next().await {
            for log in logs {
                if log.data().data.len() >= 96 {
                    let state_root = FixedBytes::<32>::from_slice(&log.data().data[0..32]);
                    let epoch = U256::from_be_slice(&log.data().data[32..64]).to::<u64>();
                    let count = U256::from_be_slice(&log.data().data[64..96]).to::<u64>();
                    
                    let event = SnapshotEvent {
                        epoch,
                        state_root,
                        count,
                    };
                    
                    let _ = handler(event).await;
                }
            }
        }

        Ok(())
    }

    pub async fn watch_claims<F, Fut>(&self, handler: F) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        F: Fn(ClaimEvent) -> Fut + Send + 'static + Clone,
        Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send,
    {
        let event_signature = "Claimed(address,uint256,bytes32)";
        let event_hash = keccak256(event_signature.as_bytes());

        let filter = Filter::new()
            .address(self.contract_address)
            .event_signature(event_hash);
        let subscription = self.provider.watch_logs(&filter).await?;
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
                    
                    let event = ClaimEvent {
                        epoch,
                        state_root,
                        claimer,
                    };
                    
                    let _ = handler(event).await;
                }
            }
        }

        Ok(())
    }
}