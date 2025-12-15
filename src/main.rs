use vea_validator::{
    epoch_watcher::EpochWatcher,
    indexer::EventIndexer,
    tasks::dispatcher::TaskDispatcher,
    contracts::IVeaInboxArbToEth,
    config::ValidatorConfig,
    startup::{check_rpc_health, check_balances},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let c = ValidatorConfig::from_env()?;
    let wallet_address = c.wallet.default_signer().address();
    println!("Validator wallet address: {}", wallet_address);

    let routes = c.build_routes();
    check_rpc_health(&routes).await?;
    check_balances(&c, &routes).await?;

    let arb_to_eth_route = routes[0].clone();
    let arb_to_gnosis_route = routes[1].clone();

    let inbox_contract = IVeaInboxArbToEth::new(arb_to_eth_route.inbox_address, arb_to_eth_route.inbox_provider.clone());
    let epoch_period: u64 = inbox_contract.epochPeriod().call().await?.try_into()?;

    let arb_to_eth_epoch_watcher = EpochWatcher::new(arb_to_eth_route.clone(), c.make_claims);
    let arb_to_gnosis_epoch_watcher = EpochWatcher::new(arb_to_gnosis_route.clone(), c.make_claims);

    let arb_to_eth_indexer = EventIndexer::new(
        arb_to_eth_route.clone(),
        "schedules/arb_to_eth.json",
        "claims/arb_to_eth.json",
    );
    let arb_to_gnosis_indexer = EventIndexer::new(
        arb_to_gnosis_route.clone(),
        "schedules/arb_to_gnosis.json",
        "claims/arb_to_gnosis.json",
    );

    let arb_to_eth_dispatcher = TaskDispatcher::new(
        c.clone(),
        arb_to_eth_route.clone(),
        "schedules/arb_to_eth.json",
        "claims/arb_to_eth.json",
    );
    let arb_to_gnosis_dispatcher = TaskDispatcher::new(
        c.clone(),
        arb_to_gnosis_route.clone(),
        "schedules/arb_to_gnosis.json",
        "claims/arb_to_gnosis.json",
    );

    println!("[ARB_TO_ETH] Inbox: {:?}, Outbox: {:?}", arb_to_eth_route.inbox_address, arb_to_eth_route.outbox_address);
    println!("[ARB_TO_GNOSIS] Inbox: {:?}, Outbox: {:?}", arb_to_gnosis_route.inbox_address, arb_to_gnosis_route.outbox_address);
    println!("Starting all handlers (3 threads per route)...");

    let h1 = tokio::spawn(async move { let _ = arb_to_eth_epoch_watcher.watch_epochs(epoch_period).await; });
    let h2 = tokio::spawn(async move { arb_to_eth_indexer.run().await });
    let h3 = tokio::spawn(async move { arb_to_eth_dispatcher.run().await });

    let h4 = tokio::spawn(async move { let _ = arb_to_gnosis_epoch_watcher.watch_epochs(epoch_period).await; });
    let h5 = tokio::spawn(async move { arb_to_gnosis_indexer.run().await });
    let h6 = tokio::spawn(async move { arb_to_gnosis_dispatcher.run().await });

    tokio::select! {
        _ = h1 => println!("ARB_TO_ETH epoch watcher stopped"),
        _ = h2 => println!("ARB_TO_ETH indexer stopped"),
        _ = h3 => println!("ARB_TO_ETH dispatcher stopped"),
        _ = h4 => println!("ARB_TO_GNOSIS epoch watcher stopped"),
        _ = h5 => println!("ARB_TO_GNOSIS indexer stopped"),
        _ = h6 => println!("ARB_TO_GNOSIS dispatcher stopped"),
        _ = tokio::signal::ctrl_c() => println!("\nShutting down..."),
    }

    Ok(())
}
