use alloy::primitives::{Address, FixedBytes, U256};
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
pub struct ClaimHandler {
    inbox_rpc: String,
    outbox_rpc: String,
    inbox_address: Address,
    outbox_address: Address,
    wallet: alloy::network::EthereumWallet,
    weth_address: Option<Address>,
    claims: Arc<RwLock<HashMap<u64, ClaimEvent>>>,
}
impl ClaimHandler {
    pub fn new(
        inbox_rpc: String,
        outbox_rpc: String,
        inbox_address: Address,
        outbox_address: Address,
        wallet: alloy::network::EthereumWallet,
        weth_address: Option<Address>,
    ) -> Self {
        Self {
            inbox_rpc,
            outbox_rpc,
            inbox_address,
            outbox_address,
            wallet,
            weth_address,
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
        use alloy::providers::ProviderBuilder;
        let provider = Arc::new(ProviderBuilder::new().connect_http(self.inbox_rpc.parse()?));
        let inbox = IVeaInboxArbToEth::new(self.inbox_address, provider);
        retry_rpc(|| async {
            inbox.snapshots(U256::from(epoch)).call().await
        }).await
    }
    pub async fn verify_claim(&self, claim: &ClaimEvent) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let correct_state_root = self.get_correct_state_root(claim.epoch).await?;
        Ok(claim.state_root == correct_state_root)
    }
    pub async fn submit_claim(&self, epoch: u64, state_root: FixedBytes<32>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use alloy::providers::ProviderBuilder;
        let provider = Arc::new(ProviderBuilder::new()
            .wallet(self.wallet.clone())
            .connect_http(self.outbox_rpc.parse()?));
        let outbox = IVeaOutboxArbToEth::new(self.outbox_address, provider);
        let deposit = retry_rpc(|| async {
            outbox.deposit().call().await
        }).await?;
        let tx = outbox.claim(U256::from(epoch), state_root)
            .from(self.wallet.default_signer().address())
            .value(deposit);
        let pending = tx.send().await?;
        let receipt = pending.get_receipt().await?;
        if !receipt.status() {
            return Err("claim transaction failed".into());
        }
        Ok(())
    }
    pub async fn challenge_claim(&self, epoch: u64, claim: IVeaOutboxArbToEth::Claim) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use alloy::providers::ProviderBuilder;
        let provider = Arc::new(ProviderBuilder::new()
            .wallet(self.wallet.clone())
            .connect_http(self.outbox_rpc.parse()?));

        if let Some(weth_addr) = self.weth_address {
            let outbox_gnosis = IVeaOutboxArbToGnosis::new(self.outbox_address, provider.clone());
            let deposit = retry_rpc(|| async {
                outbox_gnosis.deposit().call().await
            }).await?;
            let weth = IWETH::new(weth_addr, provider.clone());
            let wallet_address = self.wallet.default_signer().address();
            let current_allowance = retry_rpc(|| async {
                weth.allowance(wallet_address, self.outbox_address).call().await
            }).await?;
            if current_allowance < deposit {
                let approve_tx = weth.approve(self.outbox_address, deposit)
                    .from(wallet_address);
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
                .from(wallet_address);
            let pending = tx.send().await?;
            let receipt = pending.get_receipt().await?;
            if !receipt.status() {
                return Err("challenge transaction failed".into());
            }
        } else {
            let outbox = IVeaOutboxArbToEth::new(self.outbox_address, provider);
            let deposit = retry_rpc(|| async {
                outbox.deposit().call().await
            }).await?;
            let wallet_address = self.wallet.default_signer().address();
            let tx = outbox.challenge(U256::from(epoch), claim, wallet_address)
                .from(wallet_address)
                .value(deposit);
            let pending = tx.send().await?;
            let receipt = pending.get_receipt().await?;
            if !receipt.status() {
                return Err("challenge transaction failed".into());
            }
        }
        Ok(())
    }
    pub async fn handle_epoch_end(&self, epoch: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use alloy::providers::ProviderBuilder;
        let provider = Arc::new(ProviderBuilder::new()
            .wallet(self.wallet.clone())
            .connect_http(self.inbox_rpc.parse()?));
        let inbox = IVeaInboxArbToEth::new(self.inbox_address, provider);
        let existing_snapshot = retry_rpc(|| async {
            inbox.snapshots(U256::from(epoch)).call().await
        }).await?;
        if existing_snapshot != FixedBytes::<32>::ZERO {
            return Ok(());
        }
        let tx = inbox.saveSnapshot().from(self.wallet.default_signer().address());
        let pending = tx.send().await?;
        let receipt = pending.get_receipt().await?;
        if !receipt.status() {
            return Err("saveSnapshot transaction failed".into());
        }
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
                self.submit_claim(epoch, state_root).await?;
                Ok(())
            }
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
