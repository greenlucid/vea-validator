use alloy::primitives::U256;
use alloy::providers::Provider;
use crate::config::{Route, ValidatorConfig};
use crate::contracts::{IVeaOutboxArbToEth, IVeaOutboxArbToGnosis, IWETH};
use crate::tasks::{send_tx, ClaimStore};

pub async fn execute(
    config: &ValidatorConfig,
    route: &Route,
    epoch: u64,
    claim_store: &ClaimStore,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let claim = claim_store.get_claim(epoch);
    let wallet_address = config.wallet.default_signer().address();

    if let Some(weth_address) = route.weth_address {
        let outbox = IVeaOutboxArbToGnosis::new(route.outbox_address, route.outbox_provider.clone());
        let deposit = outbox.deposit().call().await?;

        let weth = IWETH::new(weth_address, route.outbox_provider.clone());
        let balance = weth.balanceOf(wallet_address).call().await?;
        if balance < deposit {
            println!("[{}][task::challenge] Insufficient WETH (have {}, need {}), will retry", route.name, balance, deposit);
            return Err("Insufficient funds".into());
        }

        send_tx(
            outbox.challenge(U256::from(epoch), claim).send().await,
            "challenge",
            route.name,
            &["already"],
        ).await
    } else {
        let outbox = IVeaOutboxArbToEth::new(route.outbox_address, route.outbox_provider.clone());
        let deposit = outbox.deposit().call().await?;

        let balance = route.outbox_provider.get_balance(wallet_address).await?;
        if balance < deposit {
            println!("[{}][task::challenge] Insufficient ETH (have {}, need {}), will retry", route.name, balance, deposit);
            return Err("Insufficient funds".into());
        }

        send_tx(
            outbox.challenge(U256::from(epoch), claim).value(deposit).send().await,
            "challenge",
            route.name,
            &["already"],
        ).await
    }
}
