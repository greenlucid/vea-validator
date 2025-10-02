use alloy::primitives::{Address, FixedBytes, U256};
use alloy::providers::Provider;
use crate::event_listener::ClaimEvent;
use crate::contracts::{IVeaInboxArbToEth, IVeaOutboxArbToEth};
use std::sync::Arc;

pub struct ClaimHandler<P: Provider> {
    provider: Arc<P>,
    inbox_provider: Arc<P>,
    outbox_address: Address,
    inbox_address: Address,
    wallet_address: Address,
}

impl<P: Provider> ClaimHandler<P> {
    pub fn new(
        provider: Arc<P>,
        inbox_provider: Arc<P>,
        outbox_address: Address,
        inbox_address: Address,
        wallet_address: Address,
    ) -> Self {
        Self {
            provider,
            inbox_provider,
            outbox_address,
            inbox_address,
            wallet_address,
        }
    }

    pub async fn get_claim_for_epoch(&self, epoch: u64) -> Result<Option<ClaimEvent>, Box<dyn std::error::Error + Send + Sync>> {
        let outbox = IVeaOutboxArbToEth::new(self.outbox_address, self.provider.clone());
        
        let claim_hash = outbox.claimHashes(U256::from(epoch)).call().await?;
        
        if claim_hash == FixedBytes::<32>::ZERO {
            return Ok(None);
        }
        
        panic!("get_claim_for_epoch: claim exists but event querying not implemented - this would return wrong data!");
    }

    pub async fn get_correct_state_root(&self, epoch: u64) -> Result<FixedBytes<32>, Box<dyn std::error::Error + Send + Sync>> {
        let inbox = IVeaInboxArbToEth::new(self.inbox_address, self.inbox_provider.clone());
        let state_root = inbox.snapshots(U256::from(epoch)).call().await?;
        Ok(state_root)
    }

    // Verify if a claim is correct by comparing with inbox snapshot
    pub async fn verify_claim(&self, claim: &ClaimEvent) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let correct_state_root = self.get_correct_state_root(claim.epoch).await?;
        Ok(claim.state_root == correct_state_root)
    }

    pub async fn ensure_snapshot_saved(&self, epoch: u64) -> Result<FixedBytes<32>, Box<dyn std::error::Error + Send + Sync>> {
        let inbox = IVeaInboxArbToEth::new(self.inbox_address, self.inbox_provider.clone());
        
        let existing_snapshot = inbox.snapshots(U256::from(epoch)).call().await?;
        
        if existing_snapshot != FixedBytes::<32>::ZERO {
            return Ok(existing_snapshot);
        }
        let tx = inbox.saveSnapshot().from(self.wallet_address);
        let pending = tx.send().await?;
        let receipt = pending.get_receipt().await?;
        
        if !receipt.status() {
            return Err("saveSnapshot transaction failed".into());
        }
        
        let saved_snapshot = inbox.snapshots(U256::from(epoch)).call().await?;
        Ok(saved_snapshot)
    }

    // Submit a claim to the outbox
    pub async fn submit_claim(&self, epoch: u64, state_root: FixedBytes<32>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let outbox = IVeaOutboxArbToEth::new(self.outbox_address, self.provider.clone());
        
        let deposit = outbox.deposit().call().await?;
        
        let tx = outbox.claim(U256::from(epoch), state_root)
            .from(self.wallet_address)
            .value(deposit);
            
        let pending = tx.send().await?;
        let receipt = pending.get_receipt().await?;
        
        if !receipt.status() {
            return Err("claim transaction failed".into());
        }
        
        Ok(())
    }

    // Challenge an incorrect claim
    pub async fn challenge_claim(&self, epoch: u64, claim: IVeaOutboxArbToEth::Claim) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let outbox = IVeaOutboxArbToEth::new(self.outbox_address, self.provider.clone());
        
        let deposit = outbox.deposit().call().await?;
        
        let tx = outbox.challenge(U256::from(epoch), claim, self.wallet_address)
            .from(self.wallet_address)
            .value(deposit);
            
        let pending = tx.send().await?;
        let receipt = pending.get_receipt().await?;
        
        if !receipt.status() {
            return Err("challenge transaction failed".into());
        }
        
        Ok(())
    }

    // Trigger bridge resolution by calling sendSnapshot on inbox
    pub async fn trigger_bridge_resolution(&self, epoch: u64, claim: IVeaInboxArbToEth::Claim) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let inbox = IVeaInboxArbToEth::new(self.inbox_address, self.inbox_provider.clone());
        
        let tx = inbox.sendSnapshot(U256::from(epoch), claim)
            .from(self.wallet_address);
            
        let pending = tx.send().await?;
        let receipt = pending.get_receipt().await?;
        
        if !receipt.status() {
            return Err("sendSnapshot transaction failed".into());
        }
        
        Ok(())
    }

    // Handle epoch end - decide whether to claim or challenge
    pub async fn handle_epoch_end(&self, epoch: u64) -> Result<ClaimAction, Box<dyn std::error::Error + Send + Sync>> {
        println!("Handling epoch end for epoch {}", epoch);
        
        // First ensure snapshot is saved
        let state_root = self.ensure_snapshot_saved(epoch).await?;
        
        // Check if claim already exists
        if let Some(existing_claim) = self.get_claim_for_epoch(epoch).await? {
            // Verify the claim
            let is_valid = self.verify_claim(&existing_claim).await?;
            
            if is_valid {
                println!("Existing claim for epoch {} is valid", epoch);
                Ok(ClaimAction::None)
            } else {
                println!("Existing claim for epoch {} is INVALID - should challenge", epoch);
                // In production, we'd need to reconstruct the full Claim struct from events
                Ok(ClaimAction::Challenge { 
                    epoch,
                    incorrect_claim: existing_claim,
                })
            }
        } else {
            println!("No claim exists for epoch {} - should make claim", epoch);
            Ok(ClaimAction::Claim { 
                epoch,
                state_root,
            })
        }
    }

    // Handle when a claim event is detected
    pub async fn handle_claim_event(&self, claim: ClaimEvent) -> Result<ClaimAction, Box<dyn std::error::Error + Send + Sync>> {
        println!("Handling claim event for epoch {}", claim.epoch);
        
        let is_valid = self.verify_claim(&claim).await?;
        
        if is_valid {
            println!("Claim for epoch {} is valid", claim.epoch);
            Ok(ClaimAction::None)
        } else {
            println!("Claim for epoch {} is INVALID - should challenge", claim.epoch);
            Ok(ClaimAction::Challenge {
                epoch: claim.epoch,
                incorrect_claim: claim,
            })
        }
    }
}

#[derive(Debug, Clone)]
pub enum ClaimAction {
    None,
    Claim {
        epoch: u64,
        state_root: FixedBytes<32>,
    },
    Challenge {
        epoch: u64,
        incorrect_claim: ClaimEvent,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::providers::ProviderBuilder;
    use std::str::FromStr;

    #[tokio::test]
    async fn test_handle_epoch_end_no_claim() {
        dotenv::dotenv().ok();
        
        let arbitrum_rpc = std::env::var("ARBITRUM_RPC_URL")
            .expect("ARBITRUM_RPC_URL must be set");
        let ethereum_rpc = std::env::var("ETHEREUM_RPC_URL")
            .or_else(|_| std::env::var("MAINNET_RPC_URL"))
            .expect("ETHEREUM_RPC_URL or MAINNET_RPC_URL must be set");
        
        let provider = ProviderBuilder::new()
            .connect_http(ethereum_rpc.parse().unwrap());
        let provider = Arc::new(provider);
        
        let inbox_provider = ProviderBuilder::new()
            .connect_http(arbitrum_rpc.parse().unwrap());
        let inbox_provider = Arc::new(inbox_provider);
        
        // Get actual contract addresses from env
        let outbox_address = std::env::var("VEA_OUTBOX_ARB_TO_ETH")
            .ok()
            .and_then(|s| Address::from_str(&s).ok())
            .expect("VEA_OUTBOX_ARB_TO_ETH must be set for tests");
            
        let inbox_address = Address::from_str(
            &std::env::var("VEA_INBOX_ARB_TO_ETH").expect("VEA_INBOX_ARB_TO_ETH must be set for tests")
        ).expect("Invalid VEA_INBOX_ARB_TO_ETH address");
        
        let handler = ClaimHandler::new(
            provider,
            inbox_provider,
            outbox_address,
            inbox_address,
            Address::from([0x01; 20]), // dummy wallet for tests
        );
        
        // Test that handler can be created and basic functions work
        // This mainly verifies the setup is correct
        let result = handler.get_claim_for_epoch(0).await;
        assert!(result.is_ok(), "Should be able to query epoch 0");
    }
}