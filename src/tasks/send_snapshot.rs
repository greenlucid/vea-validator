use alloy::primitives::U256;
use crate::config::Route;
use crate::contracts::{IVeaInboxArbToEth, IVeaInboxArbToGnosis};
use crate::tasks::{send_tx, ClaimStore};

pub async fn execute(
    route: &Route,
    epoch: u64,
    claim_store: &ClaimStore,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let claim = claim_store.get_claim(epoch);

    if route.weth_address.is_some() {
        let inbox = IVeaInboxArbToGnosis::new(route.inbox_address, route.inbox_provider.clone());
        let gas_limit = U256::from(500000);
        send_tx(
            inbox.sendSnapshot(U256::from(epoch), gas_limit, claim).send().await,
            "sendSnapshot",
            route.name,
            &[],
        ).await
    } else {
        let inbox = IVeaInboxArbToEth::new(route.inbox_address, route.inbox_provider.clone());
        send_tx(
            inbox.sendSnapshot(U256::from(epoch), claim).send().await,
            "sendSnapshot",
            route.name,
            &[],
        ).await
    }
}
