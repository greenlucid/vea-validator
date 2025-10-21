use alloy::primitives::U256;
use alloy::providers::{Provider, DynProvider};
use alloy::network::Ethereum;
use crate::contracts::{IVeaOutboxArbToEth, IVeaOutboxArbToGnosis, IWETH};
use crate::config::{ValidatorConfig, Route};

pub async fn check_rpc_health(routes: &[Route]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("Checking RPC endpoint health...");

    let arb_provider = &routes[0].inbox_provider;
    let eth_provider = &routes[0].outbox_provider;
    let gnosis_provider = &routes[1].outbox_provider;

    let arb_block = arb_provider.get_block_number().await
        .map_err(|e| panic!("FATAL: Arbitrum RPC unreachable or unhealthy: {}", e))?;
    println!("✓ Arbitrum RPC healthy (block: {})", arb_block);
    let eth_block = eth_provider.get_block_number().await
        .map_err(|e| panic!("FATAL: Ethereum RPC unreachable or unhealthy: {}", e))?;
    println!("✓ Ethereum RPC healthy (block: {})", eth_block);
    let gnosis_block = gnosis_provider.get_block_number().await
        .map_err(|e| panic!("FATAL: Gnosis RPC unreachable or unhealthy: {}", e))?;
    println!("✓ Gnosis RPC healthy (block: {})", gnosis_block);
    Ok(())
}

pub async fn check_balances(c: &ValidatorConfig, routes: &[Route]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let wallet = c.wallet.default_signer().address();
    let eth_provider = routes[0].outbox_provider.clone();
    let gnosis_provider = routes[1].outbox_provider.clone();

    let eth_outbox = IVeaOutboxArbToEth::new(c.outbox_arb_to_eth, eth_provider.clone());
    let gnosis_outbox = IVeaOutboxArbToGnosis::new(c.outbox_arb_to_gnosis, gnosis_provider.clone());

    let eth_deposit = eth_outbox.deposit().call().await?;
    let eth_balance = eth_provider.get_balance(wallet).await?;
    if eth_balance < eth_deposit {
        panic!("FATAL: Insufficient ETH balance. Need {} wei for deposit, have {} wei", eth_deposit, eth_balance);
    }

    let gnosis_deposit = gnosis_outbox.deposit().call().await?;
    let weth_addr = c.chains.get(&100).expect("Gnosis").deposit_token
        .expect("Gnosis should use WETH");
    let weth = IWETH::new(weth_addr, gnosis_provider.clone());
    let weth_balance = weth.balanceOf(wallet).call().await?;
    if weth_balance < gnosis_deposit {
        panic!("FATAL: Insufficient WETH balance on Gnosis. Need {} wei for deposit, have {} wei", gnosis_deposit, weth_balance);
    }
    println!("✓ Balance check passed: ETH={} wei, WETH={} wei", eth_balance, weth_balance);

    ensure_weth_approval(c, gnosis_provider).await?;

    Ok(())
}

pub async fn ensure_weth_approval(c: &ValidatorConfig, gnosis_provider: DynProvider<Ethereum>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let wallet = c.wallet.default_signer().address();
    let weth_addr = c.chains.get(&100).expect("Gnosis").deposit_token
        .expect("Gnosis should use WETH");
    let weth = IWETH::new(weth_addr, gnosis_provider);
    let current_allowance = weth.allowance(wallet, c.outbox_arb_to_gnosis).call().await?;

    if current_allowance == U256::ZERO {
        println!("⚠️  No WETH approval found for Gnosis outbox. Setting max approval...");
        let max_approval = U256::MAX;
        let approve_tx = weth.approve(c.outbox_arb_to_gnosis, max_approval).from(wallet);
        let pending = approve_tx.send().await?;
        let receipt = pending.get_receipt().await?;

        if !receipt.status() {
            panic!("FATAL: WETH approval transaction failed");
        }

        println!("✓ WETH max approval set for Gnosis outbox");
    } else {
        println!("✓ WETH approval already exists: {} wei", current_allowance);
    }

    Ok(())
}
