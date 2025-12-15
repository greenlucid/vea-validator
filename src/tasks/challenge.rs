use alloy::primitives::U256;
use crate::config::Route;
use crate::contracts::{IVeaOutboxArbToEth, IVeaOutboxArbToGnosis};
use crate::tasks::{send_tx, ClaimStore};

pub async fn execute(
    route: &Route,
    epoch: u64,
    claim_store: &ClaimStore,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let claim = claim_store.get_claim(epoch);

    if route.weth_address.is_some() {
        let outbox = IVeaOutboxArbToGnosis::new(route.outbox_address, route.outbox_provider.clone());
        send_tx(
            outbox.challenge(U256::from(epoch), claim).send().await,
            "challenge",
            route.name,
            &["Invalid claim", "already"],
        ).await
    } else {
        let outbox = IVeaOutboxArbToEth::new(route.outbox_address, route.outbox_provider.clone());
        let deposit = outbox.deposit().call().await?;
        send_tx(
            outbox.challenge(U256::from(epoch), claim).value(deposit).send().await,
            "challenge",
            route.name,
            &["Invalid claim", "already"],
        ).await
    }
}
