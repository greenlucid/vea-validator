use alloy::primitives::{Address, FixedBytes, U256};
use alloy::providers::Provider;
use crate::event_listener::ClaimEvent;
use crate::contracts::{IVeaInboxArbToEth, IVeaOutboxArbToEth, IVeaOutboxArbToGnosis, IWETH};
use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};

async fn retry_rpc<T, E, F, Fut>(mut f: F) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::error::Error + Send + Sync + 'static,
{
    for attempt in 0..5 {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) if attempt < 4 => {
                let delay = 2u64.pow(attempt);
                eprintln!("RPC call failed (attempt {}): {}, retrying in {}s...", attempt + 1, e, delay);
                sleep(Duration::from_secs(delay)).await;
            }
            Err(e) => return Err(Box::new(e)),
        }
    }
    unreachable!()
}

pub fn make_claim(event: &ClaimEvent) -> IVeaOutboxArbToEth::Claim {
    IVeaOutboxArbToEth::Claim {
        stateRoot: event.state_root,
        claimer: event.claimer,
        timestampClaimed: event.timestamp_claimed,
        timestampVerification: 0,
        blocknumberVerification: 0,
        honest: IVeaOutboxArbToEth::Party::None,
        challenger: Address::ZERO,
    }
}

async fn parse_claim_from_log<P: Provider>(
    log: &alloy::rpc::types::Log,
    provider: &Arc<P>,
) -> Result<Option<ClaimEvent>, Box<dyn std::error::Error + Send + Sync>> {
    if log.topics().len() < 3 || log.data().data.len() < 32 {
        return Ok(None);
    }

    let claimer = Address::from_slice(&log.topics()[1].0[12..]);
    let epoch = U256::from_be_bytes(log.topics()[2].0).to::<u64>();
    let state_root = FixedBytes::<32>::from_slice(&log.data().data[0..32]);

    let block_number = log.block_number.unwrap_or(0);
    let block = provider.get_block_by_number(block_number.into()).await?;
    let timestamp_claimed = block.unwrap().header.timestamp as u32;

    Ok(Some(ClaimEvent {
        epoch,
        state_root,
        claimer,
        timestamp_claimed,
    }))
}

pub struct ClaimHandler<P: Provider> {
    provider: Arc<P>,
    inbox_provider: Arc<P>,
    outbox_address: Address,
    inbox_address: Address,
    wallet_address: Address,
    weth_address: Option<Address>,
    claims: Arc<RwLock<HashMap<u64, ClaimEvent>>>,
}

impl<P: Provider> ClaimHandler<P> {
    pub fn new(
        provider: Arc<P>,
        inbox_provider: Arc<P>,
        outbox_address: Address,
        inbox_address: Address,
        wallet_address: Address,
        weth_address: Option<Address>,
    ) -> Self {
        Self {
            provider,
            inbox_provider,
            outbox_address,
            inbox_address,
            wallet_address,
            weth_address,
            claims: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn store_claim(&self, claim: ClaimEvent) {
        let mut claims = self.claims.write().await;
        claims.insert(claim.epoch, claim);
    }

    pub async fn sync_existing_claims(&self, from_epoch: u64, to_epoch: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use alloy::rpc::types::Filter;
        use alloy::primitives::keccak256;

        println!("Syncing existing claims from epoch {} to {}", from_epoch, to_epoch);

        let event_signature = "Claimed(address,uint256,bytes32)";
        let event_hash = keccak256(event_signature.as_bytes());

        let filter = Filter::new()
            .address(self.outbox_address)
            .event_signature(event_hash);

        let logs = self.provider.get_logs(&filter).await?;

        for log in logs {
            if let Some(claim) = parse_claim_from_log(&log, &self.provider).await? {
                if claim.epoch >= from_epoch && claim.epoch <= to_epoch {
                    self.store_claim(claim.clone()).await;
                    println!("Synced claim for epoch {}", claim.epoch);
                }
            }
        }
        Ok(())
    }

    pub async fn get_claim_for_epoch(&self, epoch: u64) -> Result<Option<ClaimEvent>, Box<dyn std::error::Error + Send + Sync>> {
        {
            let claims = self.claims.read().await;
            if let Some(claim) = claims.get(&epoch) {
                return Ok(Some(claim.clone()));
            }
        }
        let outbox = IVeaOutboxArbToEth::new(self.outbox_address, self.provider.clone());
        let claim_hash = retry_rpc(|| async {
            outbox.claimHashes(U256::from(epoch)).call().await
        }).await?;
        if claim_hash == FixedBytes::<32>::ZERO {
            return Ok(None);
        }
        eprintln!("CRITICAL: Claim exists for epoch {} but not in local storage. Event listener may have missed it.", epoch);
        use alloy::rpc::types::Filter;
        use alloy::primitives::keccak256;
        let event_signature = "Claimed(address,uint256,bytes32)";
        let event_hash = keccak256(event_signature.as_bytes());
        let filter = Filter::new()
            .address(self.outbox_address)
            .event_signature(event_hash)
            .topic2(U256::from(epoch));
        let logs = retry_rpc(|| async {
            self.provider.get_logs(&filter).await
        }).await?;
        if logs.is_empty() {
            panic!("FATAL: Claim hash exists for epoch {} but no Claimed event found. Blockchain state inconsistent!", epoch);
        }
        let claim = parse_claim_from_log(&logs[0], &self.provider).await?
            .ok_or_else(|| format!("FATAL: Invalid Claimed event format for epoch {}", epoch))?;
        self.store_claim(claim.clone()).await;
        Ok(Some(claim))
    }

    pub async fn get_correct_state_root(&self, epoch: u64) -> Result<FixedBytes<32>, Box<dyn std::error::Error + Send + Sync>> {
        let inbox = IVeaInboxArbToEth::new(self.inbox_address, self.inbox_provider.clone());
        retry_rpc(|| async {
            inbox.snapshots(U256::from(epoch)).call().await
        }).await
    }

    pub async fn verify_claim(&self, claim: &ClaimEvent) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let correct_state_root = self.get_correct_state_root(claim.epoch).await?;
        Ok(claim.state_root == correct_state_root)
    }

    pub async fn ensure_snapshot_saved(&self, epoch: u64) -> Result<FixedBytes<32>, Box<dyn std::error::Error + Send + Sync>> {
        let inbox = IVeaInboxArbToEth::new(self.inbox_address, self.inbox_provider.clone());
        let existing_snapshot = retry_rpc(|| async {
            inbox.snapshots(U256::from(epoch)).call().await
        }).await?;
        if existing_snapshot != FixedBytes::<32>::ZERO {
            return Ok(existing_snapshot);
        }
        let tx = inbox.saveSnapshot().from(self.wallet_address);
        let pending = tx.send().await?;
        let receipt = pending.get_receipt().await?;
        if !receipt.status() {
            return Err("saveSnapshot transaction failed".into());
        }
        let saved_snapshot = retry_rpc(|| async {
            inbox.snapshots(U256::from(epoch)).call().await
        }).await?;
        Ok(saved_snapshot)
    }
    pub async fn submit_claim(&self, epoch: u64, state_root: FixedBytes<32>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let outbox = IVeaOutboxArbToEth::new(self.outbox_address, self.provider.clone());
        let deposit = retry_rpc(|| async {
            outbox.deposit().call().await
        }).await?;
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
    pub async fn challenge_claim(&self, epoch: u64, claim: IVeaOutboxArbToEth::Claim) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(weth_addr) = self.weth_address {
            let outbox_gnosis = IVeaOutboxArbToGnosis::new(self.outbox_address, self.provider.clone());
            let deposit = retry_rpc(|| async {
                outbox_gnosis.deposit().call().await
            }).await?;
            let weth = IWETH::new(weth_addr, self.provider.clone());
            let current_allowance = retry_rpc(|| async {
                weth.allowance(self.wallet_address, self.outbox_address).call().await
            }).await?;
            if current_allowance < deposit {
                let approve_tx = weth.approve(self.outbox_address, deposit)
                    .from(self.wallet_address);
                let approve_pending = approve_tx.send().await?;
                let approve_receipt = approve_pending.get_receipt().await?;
                if !approve_receipt.status() {
                    return Err("WETH approval failed".into());
                }
            }
            let gnosis_claim = IVeaOutboxArbToGnosis::Claim {
                stateRoot: claim.stateRoot,
                claimer: claim.claimer,
                timestampClaimed: claim.timestampClaimed,
                timestampVerification: claim.timestampVerification,
                blocknumberVerification: claim.blocknumberVerification,
                honest: match claim.honest {
                    IVeaOutboxArbToEth::Party::None => IVeaOutboxArbToGnosis::Party::None,
                    IVeaOutboxArbToEth::Party::Claimer => IVeaOutboxArbToGnosis::Party::Claimer,
                    IVeaOutboxArbToEth::Party::Challenger => IVeaOutboxArbToGnosis::Party::Challenger,
                    _ => return Err("Invalid party type".into()),
                },
                challenger: claim.challenger,
            };
            let tx = outbox_gnosis.challenge(U256::from(epoch), gnosis_claim)
                .from(self.wallet_address);
            let pending = tx.send().await?;
            let receipt = pending.get_receipt().await?;
            if !receipt.status() {
                return Err("challenge transaction failed".into());
            }
        } else {
            let outbox = IVeaOutboxArbToEth::new(self.outbox_address, self.provider.clone());
            let deposit = retry_rpc(|| async {
                outbox.deposit().call().await
            }).await?;
            let tx = outbox.challenge(U256::from(epoch), claim, self.wallet_address)
                .from(self.wallet_address)
                .value(deposit);
            let pending = tx.send().await?;
            let receipt = pending.get_receipt().await?;
            if !receipt.status() {
                return Err("challenge transaction failed".into());
            }
        }
        Ok(())
    }

    pub async fn handle_epoch_end(&self, epoch: u64) -> Result<ClaimAction, Box<dyn std::error::Error + Send + Sync>> {
        println!("Handling epoch end for epoch {}", epoch);
        let state_root = self.ensure_snapshot_saved(epoch).await?;
        if let Some(existing_claim) = self.get_claim_for_epoch(epoch).await? {
            let is_valid = self.verify_claim(&existing_claim).await?;
            if is_valid {
                println!("Existing claim for epoch {} is valid", epoch);
                Ok(ClaimAction::None)
            } else {
                println!("Existing claim for epoch {} is INVALID - should challenge", epoch);
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

    pub async fn handle_claim_event(&self, claim: ClaimEvent) -> Result<ClaimAction, Box<dyn std::error::Error + Send + Sync>> {
        println!("Handling claim event for epoch {}", claim.epoch);
        self.store_claim(claim.clone()).await;
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

    pub async fn startup_sync_and_verify(&self, from_epoch: u64, to_epoch: u64) -> Result<Vec<ClaimAction>, Box<dyn std::error::Error + Send + Sync>> {
        self.sync_existing_claims(from_epoch, to_epoch).await?;
        let mut actions = Vec::new();
        for epoch in from_epoch..=to_epoch {
            if let Some(claim) = self.get_claim_for_epoch(epoch).await? {
                let is_valid = self.verify_claim(&claim).await?;
                if !is_valid {
                    actions.push(ClaimAction::Challenge {
                        epoch,
                        incorrect_claim: claim,
                    });
                }
            }
        }
        Ok(actions)
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
            None, // No WETH for ARB_TO_ETH route
        );
        
        // Test that handler can be created and basic functions work
        // This mainly verifies the setup is correct
        let result = handler.get_claim_for_epoch(0).await;
        assert!(result.is_ok(), "Should be able to query epoch 0");
    }
}