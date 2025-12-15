use alloy::primitives::U256;
use crate::config::Route;
use crate::contracts::{IVeaOutbox, Party};
use crate::tasks::{send_tx, ClaimStore};

pub async fn execute(
    route: &Route,
    epoch: u64,
    claim_store: &ClaimStore,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let claim = claim_store.get_claim(epoch);
    let outbox = IVeaOutbox::new(route.outbox_address, route.outbox_provider.clone());

    let result = match claim.honest {
        Party::Claimer => {
            send_tx(
                outbox.withdrawClaimDeposit(U256::from(epoch), claim).send().await,
                "withdrawClaimDeposit",
                route.name,
                &["already"],
            ).await
        }
        Party::Challenger => {
            send_tx(
                outbox.withdrawChallengeDeposit(U256::from(epoch), claim).send().await,
                "withdrawChallengeDeposit",
                route.name,
                &["already"],
            ).await
        }
        _ => panic!("Cannot withdraw - honest party not determined for epoch {}", epoch),
    };

    if result.is_ok() {
        claim_store.remove(epoch);
    }
    result
}
