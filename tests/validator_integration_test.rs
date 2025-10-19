use alloy::primitives::{Address, FixedBytes, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::network::Ethereum;
use serial_test::serial;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::{timeout, Duration};
use vea_validator::{
    contracts::{IVeaInboxArbToEth, IVeaOutboxArbToEth, IVeaInboxArbToGnosis, IVeaOutboxArbToGnosis, IWETH},
    event_listener::{EventListener, ClaimEvent},
    claim_handler::ClaimHandler,
    config::ValidatorConfig,
};

// Test fixture
struct TestFixture<P1: Provider, P2: Provider> {
    eth_provider: Arc<P1>,
    arb_provider: Arc<P2>,
    eth_snapshot_id: Option<String>,
    arb_snapshot_id: Option<String>,
}

impl<P1: Provider, P2: Provider> TestFixture<P1, P2> {
    fn new(eth_provider: Arc<P1>, arb_provider: Arc<P2>) -> Self {
        Self {
            eth_provider,
            arb_provider,
            eth_snapshot_id: None,
            arb_snapshot_id: None,
        }
    }

    async fn take_snapshots(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let empty_params: Vec<serde_json::Value> = vec![];

        let eth_snapshot: serde_json::Value = self.eth_provider
            .raw_request("evm_snapshot".into(), empty_params.clone())
            .await?;
        self.eth_snapshot_id = Some(eth_snapshot.as_str().unwrap().to_string());

        let arb_snapshot: serde_json::Value = self.arb_provider
            .raw_request("evm_snapshot".into(), empty_params)
            .await?;
        self.arb_snapshot_id = Some(arb_snapshot.as_str().unwrap().to_string());

        Ok(())
    }

    async fn revert_snapshots(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(ref snapshot_id) = self.eth_snapshot_id {
            let _: serde_json::Value = self.eth_provider
                .raw_request("evm_revert".into(), vec![serde_json::json!(snapshot_id)])
                .await?;
        }

        if let Some(ref snapshot_id) = self.arb_snapshot_id {
            let _: serde_json::Value = self.arb_provider
                .raw_request("evm_revert".into(), vec![serde_json::json!(snapshot_id)])
                .await?;
        }

        Ok(())
    }
}

async fn advance_time<P: Provider>(provider: &P, seconds: u64) {
    let _: serde_json::Value = provider
        .raw_request("evm_increaseTime".into(), vec![serde_json::json!(seconds)])
        .await
        .expect("Failed to advance time");

    let empty_params: Vec<serde_json::Value> = vec![];
    let _: serde_json::Value = provider
        .raw_request("evm_mine".into(), empty_params)
        .await
        .expect("Failed to mine block");
}

#[tokio::test]
#[serial]
async fn test_validator_detects_and_challenges_wrong_claim() {
    println!("\n==============================================");
    println!("VALIDATOR INTEGRATION TEST: Wrong Claim Detection");
    println!("==============================================\n");

    // Setup - use centralized config
    let c = ValidatorConfig::from_env().expect("Failed to load config");

    let providers = c.setup_arb_to_eth().expect("Failed to setup providers");

    let mut fixture = TestFixture::new(providers.destination_provider.clone(), providers.arbitrum_provider.clone());
    fixture.take_snapshots().await.unwrap();

    // STEP 1: Setup - create an epoch with messages and snapshot
    println!("--- SETUP: Creating epoch with messages and snapshot ---");
    let inbox = IVeaInboxArbToEth::new(c.inbox_arb_to_eth, providers.arbitrum_with_wallet.clone());
    let outbox = IVeaOutboxArbToEth::new(c.outbox_arb_to_eth, providers.destination_with_wallet.clone());

    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    // Send messages
    for i in 0..3 {
        let test_message = alloy::primitives::Bytes::from(vec![0xAA, 0xBB, 0xCC, i]);
        inbox.sendMessage(
            Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            test_message
        ).send().await.unwrap().get_receipt().await.unwrap();
    }

    let current_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let correct_root = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();

    if correct_root == FixedBytes::<32>::ZERO {
        panic!("Got zero root, test cannot proceed");
    }

    println!("✓ Saved snapshot for epoch {} with correct root: {:?}", current_epoch, correct_root);

    // Advance time so epoch can be claimed
    advance_time(providers.arbitrum_provider.as_ref(), epoch_period + 10).await;
    advance_time(providers.destination_provider.as_ref(), epoch_period + 10).await;

    let target_epoch = current_epoch;

    // Sync ethereum time to make the epoch claimable
    // Outbox requires: _epoch == block.timestamp / epochPeriod - 1
    let eth_block = providers.destination_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let eth_timestamp = eth_block.header.timestamp;
    let target_timestamp = (target_epoch + 1) * epoch_period + 10;
    let advance_amount = target_timestamp.saturating_sub(eth_timestamp);
    if advance_amount > 0 {
        println!("Syncing Ethereum time (advancing {} seconds)", advance_amount);
        advance_time(providers.destination_provider.as_ref(), advance_amount).await;
    }

    // STEP 2: Create the ClaimHandler (this is what the validator uses)
    println!("\n--- Starting Validator Components ---");
    let claim_handler = Arc::new(ClaimHandler::new(
        providers.destination_with_wallet.clone(),
        providers.arbitrum_with_wallet.clone(),
        c.outbox_arb_to_eth,
        c.inbox_arb_to_eth,
        providers.wallet_address,
        None, // No WETH for ARB_TO_ETH route
    ));

    // Create event listener for claims
    let event_listener = EventListener::new(
        providers.destination_provider.clone(),
        c.outbox_arb_to_eth,
    );

    // Flag to track if validator challenged
    let challenge_detected = Arc::new(AtomicBool::new(false));
    let challenge_flag = challenge_detected.clone();

    // Start watching for claims (like the validator does in main.rs)
    let claim_handler_clone = claim_handler.clone();
    let watch_handle = tokio::spawn(async move {
        event_listener.watch_claims(move |event: ClaimEvent| {
            let handler = claim_handler_clone.clone();
            let flag = challenge_flag.clone();
            Box::pin(async move {
                println!("📡 Validator detected claim for epoch {} by {}", event.epoch, event.claimer);

                // This is what the validator does in main.rs
                if let Ok(action) = handler.handle_claim_event(event.clone()).await {
                    match action {
                        vea_validator::claim_handler::ClaimAction::Challenge { epoch, incorrect_claim } => {
                            println!("⚔️  Validator decided to CHALLENGE epoch {}", epoch);

                            // Challenge the claim
                            if let Err(e) = handler.challenge_claim(epoch, vea_validator::claim_handler::make_claim(&incorrect_claim)).await {
                                eprintln!("❌ Challenge failed: {}", e);
                            } else {
                                println!("✅ Validator successfully challenged the claim!");
                                flag.store(true, Ordering::SeqCst);
                            }
                        }
                        vea_validator::claim_handler::ClaimAction::None => {
                            println!("ℹ️  Validator decided NO ACTION needed");
                        }
                        _ => {}
                    }
                }
                Ok(())
            })
        }).await
    });

    // Give the watcher time to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // STEP 3: Malicious actor makes WRONG claim
    println!("\n--- ATTACK: Malicious actor submits wrong claim ---");
    let wrong_root = FixedBytes::<32>::from([0x99; 32]);
    println!("Wrong root: {:?}", wrong_root);
    println!("Correct root: {:?}", correct_root);

    let deposit = outbox.deposit().call().await.unwrap();
    outbox.claim(U256::from(target_epoch), wrong_root)
        .value(deposit)
        .send().await.unwrap()
        .get_receipt().await.unwrap();

    println!("✓ Malicious claim submitted");

    // STEP 4: Wait for validator to react
    println!("\n--- Waiting for validator to detect and challenge... ---");

    // Give the validator up to 5 seconds to detect and challenge
    let result = timeout(Duration::from_secs(5), async {
        while !challenge_detected.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await;

    watch_handle.abort();

    if result.is_ok() {
        println!("\n✅✅✅ VALIDATOR TEST PASSED! ✅✅✅");
        println!("The validator:");
        println!("  1. Detected the malicious claim via event watching");
        println!("  2. Verified it was incorrect");
        println!("  3. Automatically challenged it");
        println!("\nThis proves the validator's reactive logic works!");
    } else {
        panic!("❌ VALIDATOR FAILED: Did not challenge the wrong claim within 5 seconds");
    }

    fixture.revert_snapshots().await.unwrap();
}

#[tokio::test]
#[serial]
async fn test_validator_triggers_bridge_resolution() {
    println!("\n==============================================");
    println!("VALIDATOR INTEGRATION TEST: Bridge Resolution");
    println!("==============================================\n");

    // Setup - use centralized config
    let c = ValidatorConfig::from_env().expect("Failed to load config");

    let providers =
        c.setup_arb_to_eth().expect("Failed to setup providers");

    let mut fixture = TestFixture::new(providers.destination_provider.clone(), providers.arbitrum_provider.clone());
    fixture.take_snapshots().await.unwrap();

    // Setup epoch with snapshot
    println!("--- SETUP: Creating epoch with messages and snapshot ---");
    let inbox = IVeaInboxArbToEth::new(c.inbox_arb_to_eth, providers.arbitrum_with_wallet.clone());
    let outbox = IVeaOutboxArbToEth::new(c.outbox_arb_to_eth, providers.destination_with_wallet.clone());

    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    for i in 0..3 {
        let test_message = alloy::primitives::Bytes::from(vec![0xDD, 0xEE, 0xFF, i]);
        inbox.sendMessage(
            Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            test_message
        ).send().await.unwrap().get_receipt().await.unwrap();
    }

    let current_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let correct_root = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();

    if correct_root == FixedBytes::<32>::ZERO {
        panic!("Got zero root, test cannot proceed");
    }

    println!("✓ Saved snapshot for epoch {}", current_epoch);

    advance_time(providers.arbitrum_provider.as_ref(), epoch_period + 10).await;
    advance_time(providers.destination_provider.as_ref(), epoch_period + 10).await;

    let target_epoch = current_epoch;

    // Sync ethereum time
    let eth_block = providers.destination_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let eth_timestamp = eth_block.header.timestamp;
    let target_timestamp = (target_epoch + 1) * epoch_period + 10;
    let advance_amount = target_timestamp.saturating_sub(eth_timestamp);
    if advance_amount > 0 {
        println!("Syncing Ethereum time (advancing {} seconds)", advance_amount);
        advance_time(providers.destination_provider.as_ref(), advance_amount).await;
    }

    // Create the bridge resolver (from main.rs)
    println!("\n--- Creating Bridge Resolver (from main.rs) ---");
    let arb_rpc_clone = c.arbitrum_rpc.clone();
    let wallet_clone = providers.wallet.clone();
    let inbox_addr = c.inbox_arb_to_eth;
    let wallet_addr = providers.wallet_address;

    let bridge_resolver_called = Arc::new(AtomicBool::new(false));
    let bridge_flag = bridge_resolver_called.clone();

    let bridge_resolver = move |epoch: u64, claim: ClaimEvent| {
        let rpc = arb_rpc_clone.clone();
        let wlt = wallet_clone.clone();
        let inbox = inbox_addr;
        let wlt_addr = wallet_addr;
        let flag = bridge_flag.clone();

        async move {
            println!("🌉 Bridge resolver triggered for epoch {}!", epoch);

            let provider = ProviderBuilder::<_, _, Ethereum>::new()
                .wallet(wlt)
                .connect_http(rpc.parse()?);
            let provider = Arc::new(provider);

            let inbox_contract = IVeaInboxArbToEth::new(inbox, provider);

            let outbox_claim = IVeaInboxArbToEth::Claim {
                stateRoot: claim.state_root,
                claimer: claim.claimer,
                timestampClaimed: claim.timestamp_claimed,
                timestampVerification: 0,
                blocknumberVerification: 0,
                honest: IVeaInboxArbToEth::Party::None,
                challenger: wlt_addr,
            };

            let tx = inbox_contract.sendSnapshot(U256::from(epoch), outbox_claim)
                .from(wlt_addr);

            let pending = tx.send().await?;
            let receipt = pending.get_receipt().await?;

            if !receipt.status() {
                return Err(Box::<dyn std::error::Error + Send + Sync>::from("sendSnapshot transaction failed"));
            }

            println!("✅ sendSnapshot called successfully! Transaction: {:?}", receipt.transaction_hash);
            flag.store(true, Ordering::SeqCst);
            Ok(())
        }
    };

    // Create claim handler with bridge resolver
    let claim_handler = Arc::new(ClaimHandler::new(
        providers.destination_with_wallet.clone(),
        providers.arbitrum_with_wallet.clone(),
        c.outbox_arb_to_eth,
        c.inbox_arb_to_eth,
        providers.wallet_address,
        None, // No WETH for ARB_TO_ETH route
    ));

    let event_listener = EventListener::new(
        providers.destination_provider.clone(),
        c.outbox_arb_to_eth,
    );

    // Watch for claims and trigger bridge resolution
    let claim_handler_clone = claim_handler.clone();
    let resolver = bridge_resolver.clone();

    let watch_handle = tokio::spawn(async move {
        event_listener.watch_claims(move |event: ClaimEvent| {
            let handler = claim_handler_clone.clone();
            let resolver_clone = resolver.clone();

            Box::pin(async move {
                println!("📡 Detected claim for epoch {}", event.epoch);

                if let Ok(action) = handler.handle_claim_event(event.clone()).await {
                    match action {
                        vea_validator::claim_handler::ClaimAction::Challenge { epoch, .. } => {
                            println!("⚔️  Challenging and triggering bridge resolution...");

                            // Challenge first
                            if let Err(e) = handler.challenge_claim(epoch, vea_validator::claim_handler::make_claim(&event)).await {
                                eprintln!("Challenge failed: {}", e);
                            } else {
                                // Then trigger bridge resolution (THIS IS THE KEY PART)
                                if let Err(e) = resolver_clone(epoch, event.clone()).await {
                                    eprintln!("Bridge resolution failed: {}", e);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(())
            })
        }).await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Malicious claim
    println!("\n--- ATTACK: Submitting wrong claim ---");
    let wrong_root = FixedBytes::<32>::from([0x88; 32]);
    let deposit = outbox.deposit().call().await.unwrap();
    outbox.claim(U256::from(target_epoch), wrong_root)
        .value(deposit)
        .send().await.unwrap()
        .get_receipt().await.unwrap();

    println!("✓ Wrong claim submitted");

    // Wait for bridge resolution
    println!("\n--- Waiting for validator to trigger bridge resolution... ---");

    let result = timeout(Duration::from_secs(5), async {
        while !bridge_resolver_called.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await;

    watch_handle.abort();

    if result.is_err() {
        panic!("❌ VALIDATOR FAILED: Did not trigger bridge resolution within 5 seconds");
    }

    println!("\n✅✅✅ BRIDGE RESOLUTION TEST PASSED! ✅✅✅");
    println!("The validator:");
    println!("  1. Detected the malicious claim");
    println!("  2. Challenged it");
    println!("  3. Automatically triggered bridge resolution via sendSnapshot");
    println!("\nThis proves the validator's complete workflow!");
    println!("Note: Full bridge message delivery (7-day delay) is not tested here");

    fixture.revert_snapshots().await.unwrap();
}

#[tokio::test]
#[serial]
async fn test_validator_detects_and_challenges_wrong_claim_arb_to_gnosis() {
    println!("\n====================================================");
    println!("VALIDATOR INTEGRATION TEST: ARB → GNOSIS Route");
    println!("====================================================\n");

    // Setup - use centralized config
    let c = ValidatorConfig::from_env().expect("Failed to load config");

    let providers =
        c.setup_arb_to_gnosis().expect("Failed to setup providers");

    let mut fixture = TestFixture::new(providers.destination_provider.clone(), providers.arbitrum_provider.clone());
    fixture.take_snapshots().await.unwrap();

    // STEP 1: Setup - create an epoch with messages and snapshot
    println!("--- SETUP: Creating epoch with messages and snapshot ---");
    let inbox = IVeaInboxArbToGnosis::new(c.inbox_arb_to_gnosis, providers.arbitrum_with_wallet.clone());
    let outbox = IVeaOutboxArbToGnosis::new(c.outbox_arb_to_gnosis, providers.destination_with_wallet.clone());

    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    // Send messages
    for i in 0..3 {
        let test_message = alloy::primitives::Bytes::from(vec![0xAA, 0xBB, 0xCC, i]);
        inbox.sendMessage(
            Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            test_message
        ).send().await.unwrap().get_receipt().await.unwrap();
    }

    let current_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let correct_root = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();

    if correct_root == FixedBytes::<32>::ZERO {
        panic!("Got zero root, test cannot proceed");
    }

    println!("✓ Saved snapshot for epoch {} with correct root: {:?}", current_epoch, correct_root);

    // Advance time so epoch can be claimed
    advance_time(providers.arbitrum_provider.as_ref(), epoch_period + 10).await;
    advance_time(providers.destination_provider.as_ref(), epoch_period + 10).await;

    let target_epoch = current_epoch;

    // Sync gnosis time to make the epoch claimable
    let gnosis_block = providers.destination_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let gnosis_timestamp = gnosis_block.header.timestamp;
    let target_timestamp = (target_epoch + 1) * epoch_period + 10;
    let advance_amount = target_timestamp.saturating_sub(gnosis_timestamp);
    if advance_amount > 0 {
        println!("Syncing Gnosis time (advancing {} seconds)", advance_amount);
        advance_time(providers.destination_provider.as_ref(), advance_amount).await;
    }

    // STEP 2: Setup WETH approval for validator (Gnosis uses WETH for deposits)
    println!("\n--- Setting up WETH for validator ---");
    let weth = IWETH::new(c.weth_gnosis, providers.destination_with_wallet.clone());

    let deposit = outbox.deposit().call().await.unwrap();
    // Mint and approve enough WETH for validator to make claims AND challenges
    weth.mintMock(providers.wallet_address, deposit * U256::from(10)).send().await.unwrap().get_receipt().await.unwrap();
    weth.approve(c.outbox_arb_to_gnosis, deposit * U256::from(10)).send().await.unwrap().get_receipt().await.unwrap();
    println!("✓ WETH minted and approved for validator");

    // STEP 3: Create the ClaimHandler (this is what the validator uses)
    println!("\n--- Starting Validator Components ---");
    let claim_handler = Arc::new(ClaimHandler::new(
        providers.destination_with_wallet.clone(),
        providers.arbitrum_with_wallet.clone(),
        c.outbox_arb_to_gnosis,
        c.inbox_arb_to_gnosis,
        providers.wallet_address,
        Some(c.weth_gnosis), // WETH for ARB_TO_GNOSIS route
    ));

    // Create event listener for claims on Gnosis
    let event_listener = EventListener::new(
        providers.destination_provider.clone(),
        c.outbox_arb_to_gnosis,
    );

    // Flag to track if validator challenged
    let challenge_detected = Arc::new(AtomicBool::new(false));
    let challenge_flag = challenge_detected.clone();

    // Start watching for claims (like the validator does in main.rs)
    let claim_handler_clone = claim_handler.clone();
    let watch_handle = tokio::spawn(async move {
        event_listener.watch_claims(move |event: ClaimEvent| {
            let handler = claim_handler_clone.clone();
            let flag = challenge_flag.clone();
            Box::pin(async move {
                println!("📡 Validator detected claim for epoch {} by {}", event.epoch, event.claimer);

                // THIS IS WHAT THE REAL VALIDATOR DOES
                if let Ok(action) = handler.handle_claim_event(event.clone()).await {
                    match action {
                        vea_validator::claim_handler::ClaimAction::Challenge { epoch, incorrect_claim } => {
                            println!("⚔️  Validator decided to CHALLENGE epoch {}", epoch);

                            // Use the REAL validator code path
                            if let Err(e) = handler.challenge_claim(epoch, vea_validator::claim_handler::make_claim(&incorrect_claim)).await {
                                eprintln!("❌ Challenge failed: {}", e);
                            } else {
                                println!("✅ Validator successfully challenged the claim!");
                                flag.store(true, Ordering::SeqCst);
                            }
                        }
                        vea_validator::claim_handler::ClaimAction::None => {
                            println!("ℹ️  Validator decided NO ACTION needed");
                        }
                        _ => {}
                    }
                }
                Ok(())
            })
        }).await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // STEP 4: Malicious actor makes wrong claim on Gnosis
    println!("\n--- ATTACK: Malicious actor submits wrong claim on Gnosis ---");

    let wrong_root = FixedBytes::<32>::from([0x99; 32]);
    println!("Wrong root: {:?}", wrong_root);
    println!("Correct root: {:?}", correct_root);

    outbox.claim(U256::from(target_epoch), wrong_root)
        .send().await.unwrap()
        .get_receipt().await.unwrap();

    println!("✓ Malicious claim submitted on Gnosis");

    // STEP 5: Wait for validator to react
    println!("\n--- Waiting for validator to detect and challenge... ---");

    let result = timeout(Duration::from_secs(5), async {
        while !challenge_detected.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await;

    watch_handle.abort();

    if result.is_ok() {
        println!("\n✅✅✅ ARB → GNOSIS VALIDATOR TEST PASSED! ✅✅✅");
        println!("The validator:");
        println!("  1. Detected the malicious claim on Gnosis via event watching");
        println!("  2. Verified it was incorrect against Arbitrum snapshot");
        println!("  3. Automatically challenged it on Gnosis");
        println!("\nThis proves the validator works for the ARB → GNOSIS route!");
    } else {
        panic!("❌ VALIDATOR FAILED: Did not challenge the wrong claim on Gnosis within 5 seconds");
    }

    fixture.revert_snapshots().await.unwrap();
}

#[tokio::test]
#[serial]
async fn test_validator_startup_sync_detects_incorrect_claim() {
    println!("\n==========================================================");
    println!("VALIDATOR INTEGRATION TEST: Startup Sync Challenge");
    println!("==========================================================\n");

    // This test verifies that when the validator restarts, it:
    // 1. Syncs existing claims from the last 10 epochs
    // 2. Verifies each synced claim
    // 3. Challenges any incorrect claims it finds

    // Setup - use centralized config
    let c = ValidatorConfig::from_env().expect("Failed to load config");

    let providers =
        c.setup_arb_to_eth().expect("Failed to setup providers");

    let mut fixture = TestFixture::new(providers.destination_provider.clone(), providers.arbitrum_provider.clone());
    fixture.take_snapshots().await.unwrap();

    // STEP 1: Create epoch with snapshot
    println!("--- SETUP: Creating epoch with messages and snapshot ---");
    let inbox = IVeaInboxArbToEth::new(c.inbox_arb_to_eth, providers.arbitrum_with_wallet.clone());
    let outbox = IVeaOutboxArbToEth::new(c.outbox_arb_to_eth, providers.destination_with_wallet.clone());

    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    // Send messages
    for i in 0..3 {
        let test_message = alloy::primitives::Bytes::from(vec![0x11, 0x22, 0x33, i]);
        inbox.sendMessage(
            Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            test_message
        ).send().await.unwrap().get_receipt().await.unwrap();
    }

    let current_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let correct_root = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();

    if correct_root == FixedBytes::<32>::ZERO {
        panic!("Got zero root, test cannot proceed");
    }

    println!("✓ Saved snapshot for epoch {} with correct root: {:?}", current_epoch, correct_root);

    // Advance time so epoch can be claimed
    advance_time(providers.arbitrum_provider.as_ref(), epoch_period + 10).await;
    advance_time(providers.destination_provider.as_ref(), epoch_period + 10).await;

    let target_epoch = current_epoch;

    // Sync ethereum time
    let eth_block = providers.destination_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let eth_timestamp = eth_block.header.timestamp;
    let target_timestamp = (target_epoch + 1) * epoch_period + 10;
    let advance_amount = target_timestamp.saturating_sub(eth_timestamp);
    if advance_amount > 0 {
        println!("Syncing Ethereum time (advancing {} seconds)", advance_amount);
        advance_time(providers.destination_provider.as_ref(), advance_amount).await;
    }

    // STEP 2: Malicious claim is made BEFORE validator starts (simulating downtime)
    println!("\n--- ATTACK: Malicious claim made while validator is DOWN ---");
    let wrong_root = FixedBytes::<32>::from([0x77; 32]);
    println!("Wrong root: {:?}", wrong_root);
    println!("Correct root: {:?}", correct_root);

    let deposit = outbox.deposit().call().await.unwrap();
    let receipt = outbox.claim(U256::from(target_epoch), wrong_root)
        .value(deposit)
        .send().await.unwrap()
        .get_receipt().await.unwrap();

    println!("✓ Malicious claim submitted at block {}", receipt.block_number.unwrap());
    println!("  (Validator was offline and missed the Claimed event)");

    // STEP 3: Now validator starts up and syncs (using REAL validator code)
    println!("\n--- VALIDATOR STARTUP: Syncing existing claims ---");
    let claim_handler = Arc::new(ClaimHandler::new(
        providers.destination_with_wallet.clone(),
        providers.arbitrum_with_wallet.clone(),
        c.outbox_arb_to_eth,
        c.inbox_arb_to_eth,
        providers.wallet_address,
        None, // No WETH for ARB_TO_ETH route
    ));

    let current_epoch_on_startup: u64 = inbox.epochFinalized().call().await.unwrap().try_into().unwrap();
    // Only sync the target epoch to avoid pollution from other tests
    let sync_from = target_epoch;
    let sync_to = target_epoch;

    println!("Current epoch: {}", current_epoch_on_startup);
    println!("Syncing claims from epoch {} to {}", sync_from, sync_to);

    // STEP 4: Call the REAL startup_sync_and_verify function from claim_handler
    // This is the exact same function that main.rs calls on startup
    println!("\n--- STARTUP VERIFICATION: Using REAL validator startup logic ---");

    let startup_actions = claim_handler.startup_sync_and_verify(sync_from, sync_to).await
        .expect("Failed to sync and verify claims");

    println!("✓ Startup sync complete, found {} actions to take", startup_actions.len());

    // STEP 5: Handle the actions (challenge incorrect claims)
    let mut challenge_triggered = false;
    for action in startup_actions {
        match action {
            vea_validator::claim_handler::ClaimAction::Challenge { epoch, incorrect_claim } => {
                println!("⚠️  Startup sync found incorrect claim for epoch {} - challenging", epoch);

                match claim_handler.challenge_claim(epoch, vea_validator::claim_handler::make_claim(&incorrect_claim)).await {
                    Ok(()) => {
                        println!("✅ Successfully challenged incorrect claim from startup sync!");
                        challenge_triggered = true;
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        if err_msg.contains("Claim already challenged") {
                            println!("ℹ️  Claim already challenged by another validator");
                            challenge_triggered = true;
                        } else {
                            panic!("❌ Challenge failed: {}", e);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // STEP 6: Verify the challenge happened
    if challenge_triggered {
        println!("\n✅✅✅ STARTUP SYNC TEST PASSED! ✅✅✅");
        println!("The validator:");
        println!("  1. Started up after being offline");
        println!("  2. Called startup_sync_and_verify() (REAL validator function)");
        println!("  3. Detected the incorrect claim that was made during downtime");
        println!("  4. Automatically challenged it on startup");
        println!("\nThis proves the validator catches up on attacks it missed during downtime!");
    } else {
        panic!("❌ TEST FAILED: Validator did not challenge the incorrect claim during startup sync");
    }

    fixture.revert_snapshots().await.unwrap();
}

#[tokio::test]
#[serial]
async fn test_validator_makes_honest_claim_when_epoch_ends() {
    println!("\n==========================================================");
    println!("VALIDATOR INTEGRATION TEST: Honest Claim on Epoch End");
    println!("==========================================================\n");

    // This test verifies that when an epoch ends with messages and no claim exists,
    // the validator REACTIVELY detects the epoch change and makes an honest claim.
    // This tests the ACTUAL validator behavior, not just calling functions directly.

    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let providers = c.setup_arb_to_eth().expect("Failed to setup providers");

    let mut fixture = TestFixture::new(providers.destination_provider.clone(), providers.arbitrum_provider.clone());
    fixture.take_snapshots().await.unwrap();

    // STEP 1: Send messages in current epoch
    let inbox = IVeaInboxArbToEth::new(c.inbox_arb_to_eth, providers.arbitrum_with_wallet.clone());
    let outbox = IVeaOutboxArbToEth::new(c.outbox_arb_to_eth, providers.destination_with_wallet.clone());

    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    for i in 0..3 {
        let test_message = alloy::primitives::Bytes::from(vec![0xDE, 0xAD, 0xBE, 0xEF, i]);
        inbox.sendMessage(
            Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            test_message
        ).send().await.unwrap().get_receipt().await.unwrap();
    }

    let starting_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();

    // STEP 2: Start validator BEFORE epoch ends
    let claim_handler = Arc::new(ClaimHandler::new(
        providers.destination_with_wallet.clone(),
        providers.arbitrum_with_wallet.clone(),
        c.outbox_arb_to_eth,
        c.inbox_arb_to_eth,
        providers.wallet_address,
        None,
    ));

    use vea_validator::epoch_watcher::EpochWatcher;
    let epoch_watcher = EpochWatcher::new(providers.arbitrum_provider.clone());

    // Flags to track validator behavior
    let claim_made = Arc::new(AtomicBool::new(false));
    let claimed_epoch = Arc::new(std::sync::RwLock::new(None::<u64>));
    let claimed_root = Arc::new(std::sync::RwLock::new(None::<FixedBytes<32>>));

    let claim_flag = claim_made.clone();
    let epoch_ref = claimed_epoch.clone();
    let root_ref = claimed_root.clone();

    let claim_handler_epoch = claim_handler.clone();
    let watcher_handle = tokio::spawn(async move {
        epoch_watcher.watch_epochs(epoch_period, move |epoch| {
            let handler = claim_handler_epoch.clone();
            Box::pin(async move {
                handler.handle_epoch_end(epoch).await.expect("Failed to save snapshot");
                Ok(())
            })
        }).await
    });

    use vea_validator::event_listener::{EventListener, SnapshotEvent};
    let event_listener = EventListener::new(providers.arbitrum_provider.clone(), c.inbox_arb_to_eth);
    let claim_handler_snapshot = claim_handler.clone();
    let snapshot_handle = tokio::spawn(async move {
        event_listener.watch_snapshots(move |event: SnapshotEvent| {
            let handler = claim_handler_snapshot.clone();
            let flag = claim_flag.clone();
            let epoch_ref = epoch_ref.clone();
            let root_ref = root_ref.clone();
            Box::pin(async move {
                if let Err(e) = handler.submit_claim(event.epoch, event.state_root).await {
                    eprintln!("Failed to submit claim: {}", e);
                } else {
                    flag.store(true, Ordering::SeqCst);
                    *epoch_ref.write().unwrap() = Some(event.epoch);
                    *root_ref.write().unwrap() = Some(event.state_root);
                }
                Ok(())
            })
        }).await
    });

    tokio::time::sleep(Duration::from_millis(500)).await;

    // STEP 3: Advance time to end epoch
    advance_time(providers.arbitrum_provider.as_ref(), epoch_period + 10).await;

    let eth_block = providers.destination_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let eth_timestamp = eth_block.header.timestamp;
    let target_timestamp = (starting_epoch + 1) * epoch_period + 10;
    let advance_amount = target_timestamp.saturating_sub(eth_timestamp);
    if advance_amount > 0 {
        advance_time(providers.destination_provider.as_ref(), advance_amount).await;
    }

    // STEP 4: Wait for validator to react

    tokio::time::sleep(Duration::from_secs(20)).await;

    watcher_handle.abort();
    snapshot_handle.abort();

    let snapshot_exists = inbox.snapshots(U256::from(starting_epoch)).call().await.unwrap();
    println!("Snapshot for epoch {}: {:?}", starting_epoch, snapshot_exists);

    if !claim_made.load(Ordering::SeqCst) {
        panic!("Validator did not make claim within 20 seconds");
    }

    let claimed_ep = claimed_epoch.read().unwrap().expect("Should have claimed an epoch");
    assert_eq!(claimed_ep, starting_epoch);

    let claim_hash = outbox.claimHashes(U256::from(starting_epoch)).call().await.unwrap();
    assert_ne!(claim_hash, FixedBytes::<32>::ZERO, "Claim should exist on-chain");

    let snapshot_root = inbox.snapshots(U256::from(starting_epoch)).call().await.unwrap();
    assert_ne!(snapshot_root, FixedBytes::<32>::ZERO, "Validator should have saved snapshot");

    let claimed_rt = claimed_root.read().unwrap().expect("Should have claimed a root");
    assert_eq!(claimed_rt, snapshot_root);

    let stored_claim = claim_handler.get_claim_for_epoch(starting_epoch).await
        .expect("Failed to get claim")
        .expect("Claim should exist");

    assert_eq!(stored_claim.state_root, snapshot_root);
    assert_eq!(stored_claim.claimer, providers.wallet_address);

    fixture.revert_snapshots().await.unwrap();
}
