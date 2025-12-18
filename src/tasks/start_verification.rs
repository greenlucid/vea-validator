use alloy::primitives::{Address, U256};
use std::sync::{Arc, Mutex};
use crate::config::Route;
use crate::contracts::IVeaOutbox;
use crate::tasks::{send_tx, was_event_emitted, ClaimStore};

pub async fn execute(
    route: &Route,
    epoch: u64,
    claim_store: &Arc<Mutex<ClaimStore>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let claim_data = claim_store.lock().unwrap().get(epoch);
    if claim_data.challenger != Address::ZERO {
        println!("[{}][task::start_verification] Epoch {} already challenged, dropping task", route.name, epoch);
        return Ok(());
    }

    let claim = claim_store.lock().unwrap().get_claim(epoch);
    let outbox = IVeaOutbox::new(route.outbox_address, route.outbox_provider.clone());
    let result = send_tx(
        outbox.startVerification(U256::from(epoch), claim).send().await,
        "startVerification",
        route.name,
    ).await;

    if let Err(e) = result {
        if was_event_emitted(&route.outbox_provider, route.outbox_address, "VerificationStarted(uint256)", epoch).await {
            println!("[{}][task::start_verification] Epoch {} already started by another validator", route.name, epoch);
            return Ok(());
        }
        if was_event_emitted(&route.outbox_provider, route.outbox_address, "Challenged(uint256,address)", epoch).await {
            println!("[{}][task::start_verification] Epoch {} was challenged, dropping task", route.name, epoch);
            return Ok(());
        }
        return Err(e);
    }
    Ok(())
}
