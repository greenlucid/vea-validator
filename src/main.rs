use std::sync::{Arc, Mutex};
use std::panic::AssertUnwindSafe;
use futures_util::future::{select_all, FutureExt};
use tokio::time::{sleep, Duration};
use vea_validator::{
    epoch_watcher::EpochWatcher,
    indexer::EventIndexer,
    tasks::dispatcher::TaskDispatcher,
    tasks::{TaskStore, ClaimStore},
    contracts::IVeaInbox,
    config::{ValidatorConfig, Route},
    startup::{check_rpc_health, check_balances, check_finality_config, load_route_settings},
};

const RETRY_DELAY_SECS: u64 = 5 * 60;

async fn run_route_with_retry(config: ValidatorConfig, route: Route) {
    loop {
        let result = AssertUnwindSafe(run_route(config.clone(), route.clone()))
            .catch_unwind()
            .await;

        match result {
            Ok(()) => break,
            Err(e) => {
                let msg = if let Some(s) = e.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = e.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                println!("[{}] Route crashed: {}. Retrying in {} seconds...", route.name, msg, RETRY_DELAY_SECS);
                sleep(Duration::from_secs(RETRY_DELAY_SECS)).await;
            }
        }
    }
}

async fn run_route(config: ValidatorConfig, route: Route) {
    let name = route.name.to_lowercase().replace("_", "-");
    let schedule_path = format!("data/schedules/{}.json", name);
    let claims_path = format!("data/claims/{}.json", name);

    let inbox = IVeaInbox::new(route.inbox_address, route.inbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await
        .expect("Failed to get epochPeriod")
        .try_into()
        .expect("epochPeriod overflow");

    println!("[{}] Inbox: {:?}, Outbox: {:?}, epochPeriod: {}s", route.name, route.inbox_address, route.outbox_address, epoch_period);

    let task_store = Arc::new(Mutex::new(TaskStore::new(&schedule_path)));
    let claim_store = Arc::new(Mutex::new(ClaimStore::new(&claims_path)));

    let wallet_address = config.wallet.default_signer().address();
    let watcher = EpochWatcher::new(config.clone(), route.clone(), config.make_claims, claim_store.clone(), task_store.clone());
    let indexer = EventIndexer::new(route.clone(), wallet_address, task_store.clone(), claim_store.clone());
    let dispatcher = TaskDispatcher::new(config, route.clone(), task_store.clone(), claim_store.clone());

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

    let mut routes = c.build_routes();
    check_rpc_health(&routes).await?;
    check_balances(&c, &routes).await?;
    check_finality_config(&c);

    let eth_provider = routes[0].outbox_provider.clone();
    for route in routes.iter_mut() {
        route.settings = load_route_settings(route, c.arb_outbox, &eth_provider).await;
    }

    println!("Starting validator for {} routes...", routes.len());

    let handles: Vec<_> = routes.into_iter()
        .map(|route| {
            let config = c.clone();
            tokio::spawn(run_route_with_retry(config, route))
        })
        .collect();

    tokio::select! {
        _ = select_all(handles) => {}
        _ = tokio::signal::ctrl_c() => {
            println!("\nShutting down...");
        }
    }

    Ok(())
}
