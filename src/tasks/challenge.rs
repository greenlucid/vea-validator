use alloy::primitives::U256;
use alloy::providers::Provider;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};
use crate::config::{Route, ValidatorConfig};
use crate::contracts::{IVeaInbox, IVeaOutboxArbToEth, IVeaOutboxArbToGnosis, IWETH};
use crate::finality::is_epoch_finalized;
use crate::tasks::{send_tx, was_event_emitted, ClaimStore};

pub async fn execute(
    config: &ValidatorConfig,
    route: &Route,
    epoch: u64,
    claim_store: &Arc<Mutex<ClaimStore>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let inbox = IVeaInbox::new(route.inbox_address, route.inbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await?.try_into()?;

    let finalized = is_epoch_finalized(
        epoch,
        epoch_period,
        &route.inbox_provider,
        &config.ethereum_provider,
        config.sequencer_inbox,
    ).await?;
    if !finalized {
        warn!(logger = "Challenge", route = route.name, epoch, "Epoch not yet finalized on L1");
        return Err("EpochNotFinalized".into());
    }

    let claim = claim_store.lock().unwrap().get_claim(epoch);
    let wallet_address = config.wallet.default_signer().address();

    let result = if let Some(weth_address) = route.weth_address {
        let outbox = IVeaOutboxArbToGnosis::new(route.outbox_address, route.outbox_provider.clone());
        let deposit = outbox.deposit().call().await?;

        let weth = IWETH::new(weth_address, route.outbox_provider.clone());
        let balance = weth.balanceOf(wallet_address).call().await?;
        if balance < deposit {
            warn!(logger = "Challenge", route = route.name, epoch, have = %balance, need = %deposit, "Insufficient WETH, will retry");
            return Err("Insufficient funds".into());
        }

        send_tx(
            outbox.challenge(U256::from(epoch), claim).send().await,
            "challenge",
            route.name,
        ).await
    } else {
        let outbox = IVeaOutboxArbToEth::new(route.outbox_address, route.outbox_provider.clone());
        let deposit = outbox.deposit().call().await?;

        let balance = route.outbox_provider.get_balance(wallet_address).await?;
        if balance < deposit {
            warn!(logger = "Challenge", route = route.name, epoch, have = %balance, need = %deposit, "Insufficient ETH, will retry");
            return Err("Insufficient funds".into());
        }

        send_tx(
            outbox.challenge(U256::from(epoch), claim).value(deposit).send().await,
            "challenge",
            route.name,
        ).await
    };

    if let Err(e) = result {
        if was_event_emitted(&route.outbox_provider, route.outbox_address, "Challenged(uint256,address)", epoch).await {
            info!(logger = "Challenge", route = route.name, epoch, "Already challenged by another validator");
            return Ok(());
        }
        if was_event_emitted(&route.outbox_provider, route.outbox_address, "VerificationStarted(uint256)", epoch).await {
            warn!(logger = "Challenge", route = route.name, epoch, "Verification started, claimHash changed - will retry");
            return Err("VerificationStarted".into());
        }
        return Err(e);
    }
    Ok(())
}
