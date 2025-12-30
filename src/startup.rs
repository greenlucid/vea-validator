use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, DynProvider};
use alloy::network::Ethereum;
use crate::contracts::{IVeaOutbox, IWETH, IOutbox, IRollup};
use crate::config::{ValidatorConfig, Route, RouteSettings};

pub fn check_finality_config(config: &ValidatorConfig) {
    if config.sequencer_inbox.is_none() {
        panic!("FATAL: SEQUENCER_INBOX must be set for L2 finality verification");
    }
    println!("✓ Finality config: sequencer_inbox={:?}", config.sequencer_inbox.unwrap());
}

const TIMING_SAFETY_BUFFER_SECS: u64 = 10 * 60;

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
    let wallet_address = c.wallet.default_signer().address();
    let eth_provider = routes[0].outbox_provider.clone();
    let gnosis_provider = routes[1].outbox_provider.clone();

    let eth_outbox = IVeaOutbox::new(c.outbox_arb_to_eth, eth_provider.clone());
    let gnosis_outbox = IVeaOutbox::new(c.outbox_arb_to_gnosis, gnosis_provider.clone());

    let eth_deposit = eth_outbox.deposit().call().await?;
    let eth_balance = eth_provider.get_balance(wallet_address).await?;
    if eth_balance < eth_deposit {
        panic!("FATAL: Insufficient ETH balance. Need {} wei for deposit, have {} wei", eth_deposit, eth_balance);
    }

    let gnosis_deposit = gnosis_outbox.deposit().call().await?;
    let weth_addr = c.chains.get(&100).expect("Gnosis").deposit_token
        .expect("Gnosis should use WETH");
    let weth = IWETH::new(weth_addr, gnosis_provider.clone());
    let weth_balance = weth.balanceOf(wallet_address).call().await?;
    if weth_balance < gnosis_deposit {
        panic!("FATAL: Insufficient WETH balance on Gnosis. Need {} wei for deposit, have {} wei", gnosis_deposit, weth_balance);
    }

    let xdai_balance = gnosis_provider.get_balance(wallet_address).await?;
    let xdai_min = U256::from(10_000_000_000_000_000u64);
    if xdai_balance < xdai_min {
        panic!("FATAL: Insufficient xDAI on Gnosis for gas. Need {} wei, have {} wei", xdai_min, xdai_balance);
    }
    println!("✓ Balance check passed: ETH={} wei, WETH={} wei, xDAI={} wei", eth_balance, weth_balance, xdai_balance);

    ensure_weth_approval(c, gnosis_provider, wallet_address).await?;

    Ok(())
}

pub async fn ensure_weth_approval(c: &ValidatorConfig, gnosis_provider: DynProvider<Ethereum>, wallet_address: alloy::primitives::Address) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let weth_addr = c.chains.get(&100).expect("Gnosis").deposit_token
        .expect("Gnosis should use WETH");
    let weth = IWETH::new(weth_addr, gnosis_provider);
    let current_allowance = weth.allowance(wallet_address, c.outbox_arb_to_gnosis).call().await?;

    if current_allowance == U256::ZERO {
        println!("⚠️  No WETH approval found for Gnosis outbox. Setting max approval...");
        let max_approval = U256::MAX;
        let approve_tx = weth.approve(c.outbox_arb_to_gnosis, max_approval);
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

async fn get_avg_block_time_ms(provider: &DynProvider<Ethereum>) -> u64 {
    let latest = provider.get_block_number().await
        .expect("Failed to get latest block number");
    let latest_block = provider.get_block_by_number(latest.into()).await
        .expect("Failed to get latest block")
        .expect("Latest block not found");
    let old_block = provider.get_block_by_number((latest - 10000).into()).await
        .expect("Failed to get old block")
        .expect("Old block not found");

    let time_diff = latest_block.header.timestamp - old_block.header.timestamp;
    (time_diff * 1000) / 10000
}

pub async fn load_route_settings(
    route: &Route,
    arb_outbox_address: Address,
    arb_outbox_provider: &DynProvider<Ethereum>,
) -> RouteSettings {
    println!("[{}] Loading route settings from contracts...", route.name);

    let avg_block_time_ms = get_avg_block_time_ms(arb_outbox_provider).await;
    println!("[{}] Average block time: {}ms", route.name, avg_block_time_ms);

    let arb_outbox = IOutbox::new(arb_outbox_address, arb_outbox_provider.clone());
    let rollup_address = arb_outbox.rollup().call().await
        .expect("Failed to get rollup address from Arbitrum outbox");
    let rollup = IRollup::new(rollup_address, arb_outbox_provider.clone());
    let confirm_period_blocks: u64 = rollup.confirmPeriodBlocks().call().await
        .expect("Failed to get confirmPeriodBlocks")
        .max(14458);
    println!("[{}] Rollup confirmPeriodBlocks: {}", route.name, confirm_period_blocks);

    let outbox = IVeaOutbox::new(route.outbox_address, route.outbox_provider.clone());
    let sequencer_delay_limit = outbox.sequencerDelayLimit().call().await.expect("Failed to get sequencerDelayLimit").to::<u64>();
    let min_challenge_period = outbox.minChallengePeriod().call().await.expect("Failed to get minChallengePeriod").to::<u64>();
    let epoch_period = outbox.epochPeriod().call().await.expect("Failed to get epochPeriod").to::<u64>();
    println!("[{}] Outbox params: sequencerDelayLimit={}s, epochPeriod={}s, minChallengePeriod={}s",
        route.name, sequencer_delay_limit, epoch_period, min_challenge_period);

    let relay_delay_secs = (confirm_period_blocks * avg_block_time_ms / 1000) + TIMING_SAFETY_BUFFER_SECS;
    let start_verification_delay = sequencer_delay_limit + epoch_period + TIMING_SAFETY_BUFFER_SECS;
    let min_challenge_period_with_buffer = min_challenge_period + TIMING_SAFETY_BUFFER_SECS;
    let sync_lookback_secs = relay_delay_secs + start_verification_delay + min_challenge_period_with_buffer + TIMING_SAFETY_BUFFER_SECS;

    println!("[{}] Computed: relay_delay={}s, start_verification_delay={}s, min_challenge_period={}s, sync_lookback={}s",
        route.name, relay_delay_secs, start_verification_delay, min_challenge_period_with_buffer, sync_lookback_secs);

    RouteSettings {
        relay_delay_secs,
        start_verification_delay,
        min_challenge_period: min_challenge_period_with_buffer,
        sync_lookback_secs,
    }
}
