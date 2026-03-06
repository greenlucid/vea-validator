use alloy::primitives::{FixedBytes, U256};
use std::sync::{Arc, Mutex};
use tracing::info;
use crate::config::Route;
use crate::contracts::{IVeaOutbox, Party};
use crate::tasks::{send_tx, ClaimStore};

pub async fn execute(
    route: &Route,
    epoch: u64,
    claim_store: &Arc<Mutex<ClaimStore>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outbox = IVeaOutbox::new(route.outbox_address, route.outbox_provider.clone());

    let claim_hash = outbox.claimHashes(U256::from(epoch)).call().await?;
    if claim_hash == FixedBytes::<32>::ZERO {
        info!(logger = "WithdrawDeposit", route = route.name, epoch, "Already withdrawn");
        claim_store.lock().unwrap().remove(epoch);
        return Ok(());
    }

    let claim = claim_store.lock().unwrap().get_claim(epoch);
    info!(logger = "WithdrawDeposit", route = route.name, epoch, honest = ?claim.honest, "Withdrawing deposit");

    let result = match claim.honest {
        Party::Claimer => {
            send_tx(
                outbox.withdrawClaimDeposit(U256::from(epoch), claim).send().await,
                "withdrawClaimDeposit",
                route.name,
            ).await
        }
        Party::Challenger => {
            send_tx(
                outbox.withdrawChallengeDeposit(U256::from(epoch), claim).send().await,
                "withdrawChallengeDeposit",
                route.name,
            ).await
        }
        _ => panic!("Cannot withdraw - honest party not determined for epoch {}", epoch),
    };

    if let Err(e) = result {
        let claim_hash = outbox.claimHashes(U256::from(epoch)).call().await?;
        if claim_hash == FixedBytes::<32>::ZERO {
            info!(logger = "WithdrawDeposit", route = route.name, epoch, "Already withdrawn by another validator");
            claim_store.lock().unwrap().remove(epoch);
            return Ok(());
        }
        return Err(e);
    }

    claim_store.lock().unwrap().remove(epoch);
    Ok(())
}
