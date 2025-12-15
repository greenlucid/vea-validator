use alloy::primitives::U256;
use crate::config::Route;
use crate::contracts::IVeaOutbox;
use crate::tasks::{send_tx, ClaimStore};

pub async fn execute(
    route: &Route,
    epoch: u64,
    claim_store: &ClaimStore,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let claim = claim_store.get_claim(epoch);

    let outbox = IVeaOutbox::new(route.outbox_address, route.outbox_provider.clone());
    send_tx(
        outbox.startVerification(U256::from(epoch), claim).send().await,
        "startVerification",
        route.name,
        &["already"],
    ).await
}
