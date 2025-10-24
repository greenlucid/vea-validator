use alloy::primitives::{Address, FixedBytes, U256};
use alloy::providers::Provider;
use crate::event_listener::ClaimEvent;
use crate::contracts::{IVeaInboxArbToEth, IVeaOutboxArbToEth, IVeaOutboxArbToGnosis, IWETH, Claim, Party};
use crate::config::Route;
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
#[inline]
pub fn make_claim(event: &ClaimEvent) -> Claim {
    Claim {
        stateRoot: event.state_root,
        claimer: event.claimer,
        timestampClaimed: event.timestamp_claimed,
        timestampVerification: 0,
        blocknumberVerification: 0,
        honest: Party::None,
        challenger: Address::ZERO,
    }
}
pub struct ClaimHandler {
    route: Route,
    wallet_address: Address,
    claims: Arc<RwLock<HashMap<u64, ClaimEvent>>>,
}
impl ClaimHandler {
    pub fn new(route: Route, wallet_address: Address) -> Self {
        Self {
            route,
            wallet_address,
            claims: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    pub async fn store_claim(&self, claim: ClaimEvent) {
        let mut claims = self.claims.write().await;
        claims.insert(claim.epoch, claim);
    }
    pub async fn get_claim_for_epoch(&self, epoch: u64) -> Result<Option<ClaimEvent>, Box<dyn std::error::Error + Send + Sync>> {
        let claims = self.claims.read().await;
        Ok(claims.get(&epoch).cloned())
    }
    pub async fn get_correct_state_root(&self, epoch: u64) -> Result<FixedBytes<32>, Box<dyn std::error::Error + Send + Sync>> {
        let inbox = IVeaInboxArbToEth::new(self.route.inbox_address, self.route.inbox_provider.clone());
        retry_rpc(|| async {
            inbox.snapshots(U256::from(epoch)).call().await
        }).await
    }
    pub async fn verify_claim(&self, claim: &ClaimEvent) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let correct_state_root = self.get_correct_state_root(claim.epoch).await?;
        Ok(claim.state_root == correct_state_root)
    }
    pub async fn submit_claim(&self, epoch: u64, state_root: FixedBytes<32>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let outbox = IVeaOutboxArbToEth::new(self.route.outbox_address, self.route.outbox_provider.clone());
        let deposit = retry_rpc(|| async {
            outbox.deposit().call().await
        }).await?;
        let tx = outbox.claim(U256::from(epoch), state_root)
            .value(deposit);
        let pending = match tx.send().await {
            Ok(p) => p,
            Err(e) => {
                let err_msg = e.to_string();
                if err_msg.contains("Claim already made") {
                    return Err("Claim already made".into());
                }
                panic!("FATAL: Unexpected error submitting claim for epoch {}: {}", epoch, e);
            }
        };
        let receipt = pending.get_receipt().await?;
        if !receipt.status() {
            panic!("FATAL: Claim transaction reverted for epoch {}", epoch);
        }
        Ok(())
    }
    pub async fn challenge_claim(&self, epoch: u64, claim: Claim) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(weth_addr) = self.route.weth_address {
            let outbox_gnosis = IVeaOutboxArbToGnosis::new(self.route.outbox_address, self.route.outbox_provider.clone());
            let deposit = retry_rpc(|| async {
                outbox_gnosis.deposit().call().await
            }).await?;
            let weth = IWETH::new(weth_addr, self.route.outbox_provider.clone());
            let current_allowance = retry_rpc(|| async {
                weth.allowance(self.wallet_address, self.route.outbox_address).call().await
            }).await?;
            if current_allowance < deposit {
                let approve_tx = weth.approve(self.route.outbox_address, deposit);
                let approve_pending = approve_tx.send().await?;
                let approve_receipt = approve_pending.get_receipt().await?;
                if !approve_receipt.status() {
                    return Err("WETH approval failed".into());
                }
            }
            let tx = outbox_gnosis.challenge(U256::from(epoch), claim);
            let pending = match tx.send().await {
                Ok(p) => p,
                Err(e) => {
                    let err_msg = e.to_string();
                    if err_msg.contains("Invalid claim") {
                        return Err("Claim already challenged".into());
                    }
                    panic!("FATAL: Unexpected error challenging claim for epoch {}: {}", epoch, e);
                }
            };
            let receipt = pending.get_receipt().await?;
            if !receipt.status() {
                panic!("FATAL: Challenge transaction reverted for epoch {}", epoch);
            }
        } else {
            let outbox = IVeaOutboxArbToEth::new(self.route.outbox_address, self.route.outbox_provider.clone());
            let deposit = retry_rpc(|| async {
                outbox.deposit().call().await
            }).await?;
            let tx = outbox.challenge(U256::from(epoch), claim, self.wallet_address)
                .value(deposit);
            let pending = match tx.send().await {
                Ok(p) => p,
                Err(e) => {
                    let err_msg = e.to_string();
                    if err_msg.contains("Invalid claim") {
                        return Err("Claim already challenged".into());
                    }
                    panic!("FATAL: Unexpected error challenging claim for epoch {}: {}", epoch, e);
                }
            };
            let receipt = pending.get_receipt().await?;
            if !receipt.status() {
                panic!("FATAL: Challenge transaction reverted for epoch {}", epoch);
            }
        }
        Ok(())
    }
    pub async fn handle_epoch_end(&self, epoch: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let inbox = IVeaInboxArbToEth::new(self.route.inbox_address, self.route.inbox_provider.clone());
        let epoch_period = retry_rpc(|| async { inbox.epochPeriod().call().await }).await?.to::<u64>();
        let epoch_start_ts = epoch * epoch_period;
        let current_block = self.route.inbox_provider.get_block_number().await?;
        let current_ts = self.route.inbox_provider.get_block_by_number(current_block.into()).await?.unwrap().header.timestamp;
        let elapsed_ms = (current_ts - epoch_start_ts) * 1000;
        let from_block = current_block.saturating_sub(elapsed_ms * 110 / 100 / self.route.inbox_avg_block_millis as u64);

        let msg_sent_sig = alloy::primitives::keccak256("MessageSent(bytes)".as_bytes());
        let snapshot_saved_sig = alloy::primitives::keccak256("SnapshotSaved(bytes32,uint256,uint64)".as_bytes());

        let msg_filter = alloy::rpc::types::Filter::new().address(self.route.inbox_address).event_signature(msg_sent_sig).from_block(from_block);
        let snapshot_filter = alloy::rpc::types::Filter::new().address(self.route.inbox_address).event_signature(snapshot_saved_sig).from_block(from_block);

        let (msg_logs, snapshot_logs) = tokio::join!(
            self.route.inbox_provider.get_logs(&msg_filter),
            self.route.inbox_provider.get_logs(&snapshot_filter)
        );

        if msg_logs?.is_empty() { return Ok(()); }

        if let Some(last_snapshot) = snapshot_logs?.last() {
            if last_snapshot.data().data.len() >= 96 {
                let saved_count = U256::from_be_slice(&last_snapshot.data().data[64..96]).to::<u64>();
                let current_count = retry_rpc(|| async { inbox.count().call().await }).await?;
                if saved_count == current_count { return Ok(()); }
            }
        }

        let tx = inbox.saveSnapshot();
        let pending = tx.send().await?;
        let receipt = pending.get_receipt().await?;
        if !receipt.status() { return Err("saveSnapshot transaction failed".into()); }
        Ok(())
    }
    pub async fn handle_after_epoch_start(&self, epoch: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let state_root = self.get_correct_state_root(epoch).await?;
        if state_root == FixedBytes::<32>::ZERO {
            return Ok(());
        }
        match self.get_claim_for_epoch(epoch).await? {
            Some(_) => Ok(()),
            None => {
                match self.submit_claim(epoch, state_root).await {
                    Ok(()) => Ok(()),
                    Err(_) => {
                        println!("Claim already made by another validator for epoch {} - bridge is safe", epoch);
                        Ok(())
                    }
                }
            }
        }
    }
    pub async fn handle_claim_event(&self, claim: ClaimEvent) -> Result<Option<ClaimEvent>, Box<dyn std::error::Error + Send + Sync>> {
        println!("Handling claim event for epoch {}", claim.epoch);
        self.store_claim(claim.clone()).await;
        let is_valid = self.verify_claim(&claim).await?;
        if is_valid {
            println!("Claim for epoch {} is valid", claim.epoch);
            Ok(None)
        } else {
            println!("Claim for epoch {} is INVALID - should challenge", claim.epoch);
            Ok(Some(claim))
        }
    }
}
