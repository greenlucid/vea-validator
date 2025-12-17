use futures_util::future::select_all;
use vea_validator::{
    epoch_watcher::EpochWatcher,
    indexer::EventIndexer,
    tasks::dispatcher::TaskDispatcher,
    contracts::IVeaInboxArbToEth,
    config::{ValidatorConfig, Route},
    startup::{check_rpc_health, check_balances},
};

async fn run_route(config: ValidatorConfig, route: Route, epoch_period: u64) {
    let name = route.name.to_lowercase().replace("_", "-");
    let schedule_path = format!("schedules/{}.json", name);
    let claims_path = format!("claims/{}.json", name);

    println!("[{}] Inbox: {:?}, Outbox: {:?}", route.name, route.inbox_address, route.outbox_address);

    let wallet_address = config.wallet.default_signer().address();
    let watcher = EpochWatcher::new(route.clone(), config.make_claims, &claims_path, &schedule_path);
    let indexer = EventIndexer::new(route.clone(), wallet_address, &schedule_path, &claims_path);
    let dispatcher = TaskDispatcher::new(config, route.clone(), &schedule_path, &claims_path);

    indexer.initialize().await;

    tokio::select! {
        r = watcher.watch_epochs(epoch_period) => {
            panic!("[{}] Epoch watcher died: {:?}", route.name, r);
        }
        _ = indexer.run() => {
            panic!("[{}] Indexer died unexpectedly", route.name);
        }
        _ = dispatcher.run() => {
            panic!("[{}] Dispatcher died unexpectedly", route.name);
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let c = ValidatorConfig::from_env()?;
    println!("Validator wallet address: {}", c.wallet.default_signer().address());

    let routes = c.build_routes();
    check_rpc_health(&routes).await?;
    check_balances(&c, &routes).await?;

    let inbox = IVeaInboxArbToEth::new(routes[0].inbox_address, routes[0].inbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await?.try_into()?;

    println!("Starting validator for {} routes...", routes.len());

    let handles: Vec<_> = routes.into_iter()
        .map(|route| {
            let config = c.clone();
            tokio::spawn(run_route(config, route, epoch_period))
        })
        .collect();

    tokio::select! {
        _ = select_all(handles) => {
            panic!("A route handler died unexpectedly");
        }
        _ = tokio::signal::ctrl_c() => {
            println!("\nShutting down...");
        }
    }

    Ok(())
}
