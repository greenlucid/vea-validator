use alloy::signers::local::PrivateKeySigner;
use alloy::network::EthereumWallet;
use std::str::FromStr;
use std::sync::Arc;
use vea_validator::{
    epoch_watcher::EpochWatcher,
    claim_handler::ClaimHandler,
    contracts::IVeaInboxArbToEth,
    config::ValidatorConfig,
    l2_to_l1_finder::L2ToL1Finder,
    arb_relay_handler::ArbRelayHandler,
    claim_finder::ClaimFinder,
    verification_handler::VerificationHandler,
    startup::{check_rpc_health, check_balances},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let c = ValidatorConfig::from_env()?;
    let signer = PrivateKeySigner::from_str(&c.private_key)?;
    let wallet = EthereumWallet::from(signer);
    let wallet_address = wallet.default_signer().address();
    println!("Validator wallet address: {}", wallet_address);

    let routes = c.build_routes();
    check_rpc_health(&routes).await?;
    check_balances(&c, &routes).await?;

    let arb_to_eth_route = &routes[0];
    let arb_to_gnosis_route = &routes[1];

    let inbox_contract = IVeaInboxArbToEth::new(arb_to_eth_route.inbox_address, arb_to_eth_route.inbox_provider.clone());
    let epoch_period: u64 = inbox_contract.epochPeriod().call().await?.try_into()?;

    let arb_to_eth_claim_handler = Arc::new(ClaimHandler::new(arb_to_eth_route.clone(), wallet_address));
    let arb_to_gnosis_claim_handler = Arc::new(ClaimHandler::new(arb_to_gnosis_route.clone(), wallet_address));

    let arb_to_eth_epoch_watcher = EpochWatcher::new(
        arb_to_eth_route.inbox_provider.clone(),
        arb_to_eth_claim_handler.clone(),
        "ARB_TO_ETH",
    );
    let arb_to_gnosis_epoch_watcher = EpochWatcher::new(
        arb_to_gnosis_route.inbox_provider.clone(),
        arb_to_gnosis_claim_handler.clone(),
        "ARB_TO_GNOSIS",
    );

    let l2_to_l1_finder = L2ToL1Finder::new(arb_to_eth_route.inbox_provider.clone())
        .add_inbox(arb_to_eth_route.inbox_address, "schedules/arb_to_eth_relay.json")
        .add_inbox(arb_to_gnosis_route.inbox_address, "schedules/arb_to_gnosis_relay.json");

    let arb_to_eth_relay_handler = ArbRelayHandler::new(
        arb_to_eth_route.inbox_provider.clone(),
        arb_to_eth_route.outbox_provider.clone(),
        c.arb_outbox,
        "schedules/arb_to_eth_relay.json",
    );

    let arb_to_gnosis_relay_handler = ArbRelayHandler::new(
        arb_to_gnosis_route.inbox_provider.clone(),
        arb_to_eth_route.outbox_provider.clone(),
        c.arb_outbox,
        "schedules/arb_to_gnosis_relay.json",
    );

    let arb_to_eth_claim_finder = ClaimFinder::new(
        arb_to_eth_route.inbox_provider.clone(),
        arb_to_eth_route.outbox_provider.clone(),
        arb_to_eth_route.inbox_address,
        arb_to_eth_route.outbox_address,
        None,
        wallet_address,
        "schedules/arb_to_eth_verification.json",
        "schedules/arb_to_eth_claims.json",
        "ARB_TO_ETH",
    );

    let arb_to_gnosis_claim_finder = ClaimFinder::new(
        arb_to_gnosis_route.inbox_provider.clone(),
        arb_to_gnosis_route.outbox_provider.clone(),
        arb_to_gnosis_route.inbox_address,
        arb_to_gnosis_route.outbox_address,
        arb_to_gnosis_route.weth_address,
        wallet_address,
        "schedules/arb_to_gnosis_verification.json",
        "schedules/arb_to_gnosis_claims.json",
        "ARB_TO_GNOSIS",
    );

    let arb_to_eth_verification_handler = VerificationHandler::new(
        arb_to_eth_route.outbox_provider.clone(),
        arb_to_eth_route.outbox_address,
        None,
        "schedules/arb_to_eth_verification.json",
        "ARB_TO_ETH",
    );

    let arb_to_gnosis_verification_handler = VerificationHandler::new(
        arb_to_gnosis_route.outbox_provider.clone(),
        arb_to_gnosis_route.outbox_address,
        arb_to_gnosis_route.weth_address,
        "schedules/arb_to_gnosis_verification.json",
        "ARB_TO_GNOSIS",
    );

    println!("[ARB_TO_ETH] Inbox: {:?}, Outbox: {:?}", arb_to_eth_route.inbox_address, arb_to_eth_route.outbox_address);
    println!("[ARB_TO_GNOSIS] Inbox: {:?}, Outbox: {:?}", arb_to_gnosis_route.inbox_address, arb_to_gnosis_route.outbox_address);
    println!("Starting all handlers...");

    let h1 = tokio::spawn(async move { let _ = arb_to_eth_epoch_watcher.watch_epochs(epoch_period).await; });
    let h2 = tokio::spawn(async move { let _ = arb_to_gnosis_epoch_watcher.watch_epochs(epoch_period).await; });
    let h3 = tokio::spawn(async move { l2_to_l1_finder.run().await });
    let h4 = tokio::spawn(async move { arb_to_eth_relay_handler.run().await });
    let h5 = tokio::spawn(async move { arb_to_gnosis_relay_handler.run().await });
    let h6 = tokio::spawn(async move { arb_to_eth_claim_finder.run().await });
    let h7 = tokio::spawn(async move { arb_to_gnosis_claim_finder.run().await });
    let h8 = tokio::spawn(async move { arb_to_eth_verification_handler.run().await });
    let h9 = tokio::spawn(async move { arb_to_gnosis_verification_handler.run().await });

    tokio::select! {
        _ = h1 => println!("ARB_TO_ETH epoch watcher stopped"),
        _ = h2 => println!("ARB_TO_GNOSIS epoch watcher stopped"),
        _ = h3 => println!("L2ToL1 finder stopped"),
        _ = h4 => println!("ARB_TO_ETH relay handler stopped"),
        _ = h5 => println!("ARB_TO_GNOSIS relay handler stopped"),
        _ = h6 => println!("ARB_TO_ETH claim finder stopped"),
        _ = h7 => println!("ARB_TO_GNOSIS claim finder stopped"),
        _ = h8 => println!("ARB_TO_ETH verification handler stopped"),
        _ = h9 => println!("ARB_TO_GNOSIS verification handler stopped"),
        _ = tokio::signal::ctrl_c() => println!("\nShutting down..."),
    }

    Ok(())
}
