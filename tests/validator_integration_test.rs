mod common;

use alloy::primitives::{Address, FixedBytes, U256};
use alloy::providers::ProviderBuilder;
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
    epoch_watcher::EpochWatcher,
};
use common::{TestFixture, advance_time, Provider};

#[tokio::test]
#[serial]
async fn test_validator_detects_and_challenges_wrong_claim() {
    println!("\n==============================================");
    println!("VALIDATOR INTEGRATION TEST: Wrong Claim Detection");
    println!("==============================================\n");

    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let route = &routes[0];

    let inbox_provider = Arc::new(route.inbox_provider.clone());
    let outbox_provider = Arc::new(route.outbox_provider.clone());

    let mut fixture = TestFixture::new(outbox_provider.clone(), inbox_provider.clone());
    fixture.take_snapshots().await.unwrap();

    println!("--- SETUP: Creating epoch with messages and snapshot ---");
    let inbox = IVeaInboxArbToEth::new(route.inbox_address, inbox_provider.clone());
    let outbox = IVeaOutboxArbToEth::new(route.outbox_address, outbox_provider.clone());

    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

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

    advance_time(inbox_provider.as_ref(), epoch_period + 10).await;
    advance_time(outbox_provider.as_ref(), epoch_period + 10).await;

    let target_epoch = current_epoch;

    let eth_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let eth_timestamp = eth_block.header.timestamp;
    let target_timestamp = (target_epoch + 1) * epoch_period + 10;
    let advance_amount = target_timestamp.saturating_sub(eth_timestamp);
    if advance_amount > 0 {
        println!("Syncing Ethereum time (advancing {} seconds)", advance_amount);
        advance_time(outbox_provider.as_ref(), advance_amount).await;
    }

    println!("\n--- Starting Validator Components ---");
    let claim_handler = Arc::new(ClaimHandler::new(route.clone(), c.wallet.clone()));

    let event_listener = EventListener::new(route.outbox_provider.clone(), route.outbox_address);

    let challenge_detected = Arc::new(AtomicBool::new(false));
    let challenge_flag = challenge_detected.clone();

    let claim_handler_clone = claim_handler.clone();
    let watch_handle = tokio::spawn(async move {
        event_listener.watch_claims(move |event: ClaimEvent| {
            let handler = claim_handler_clone.clone();
            let flag = challenge_flag.clone();
            Box::pin(async move {
                println!("📡 Validator detected claim for epoch {} by {}", event.epoch, event.claimer);

                if let Ok(action) = handler.handle_claim_event(event.clone()).await {
                    match action {
                        vea_validator::claim_handler::ClaimAction::Challenge { epoch, incorrect_claim } => {
                            println!("⚔️  Validator decided to CHALLENGE epoch {}", epoch);

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

    println!("\n--- Waiting for validator to detect and challenge... ---");

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

    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let route = &routes[0];

    let inbox_provider = Arc::new(route.inbox_provider.clone());
    let outbox_provider = Arc::new(route.outbox_provider.clone());

    let mut fixture = TestFixture::new(outbox_provider.clone(), inbox_provider.clone());
    fixture.take_snapshots().await.unwrap();

    println!("--- SETUP: Creating epoch with messages and snapshot ---");
    let inbox = IVeaInboxArbToEth::new(route.inbox_address, inbox_provider.clone());
    let outbox = IVeaOutboxArbToEth::new(route.outbox_address, outbox_provider.clone());

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

    advance_time(inbox_provider.as_ref(), epoch_period + 10).await;
    advance_time(outbox_provider.as_ref(), epoch_period + 10).await;

    let target_epoch = current_epoch;

    let eth_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let eth_timestamp = eth_block.header.timestamp;
    let target_timestamp = (target_epoch + 1) * epoch_period + 10;
    let advance_amount = target_timestamp.saturating_sub(eth_timestamp);
    if advance_amount > 0 {
        println!("Syncing Ethereum time (advancing {} seconds)", advance_amount);
        advance_time(outbox_provider.as_ref(), advance_amount).await;
    }

    println!("\n--- Creating Bridge Resolver (from main.rs) ---");
    let wallet_address = c.wallet.default_signer().address();
    let inbox_provider_clone = route.inbox_provider.clone();
    let inbox_addr = route.inbox_address;

    let bridge_resolver_called = Arc::new(AtomicBool::new(false));
    let bridge_flag = bridge_resolver_called.clone();

    let bridge_resolver = move |epoch: u64, claim: ClaimEvent| {
        let provider = inbox_provider_clone.clone();
        let inbox = inbox_addr;
        let wlt_addr = wallet_address;
        let flag = bridge_flag.clone();

        async move {
            println!("🌉 Bridge resolver triggered for epoch {}!", epoch);

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

    let claim_handler = Arc::new(ClaimHandler::new(route.clone(), c.wallet.clone()));

    let event_listener = EventListener::new(route.outbox_provider.clone(), route.outbox_address);

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

                            if let Err(e) = handler.challenge_claim(epoch, vea_validator::claim_handler::make_claim(&event)).await {
                                eprintln!("Challenge failed: {}", e);
                            } else {
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

    println!("\n--- ATTACK: Submitting wrong claim ---");
    let wrong_root = FixedBytes::<32>::from([0x88; 32]);
    let deposit = outbox.deposit().call().await.unwrap();
    outbox.claim(U256::from(target_epoch), wrong_root)
        .value(deposit)
        .send().await.unwrap()
        .get_receipt().await.unwrap();

    println!("✓ Wrong claim submitted");

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

    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let route = &routes[1];

    let inbox_provider = Arc::new(route.inbox_provider.clone());
    let outbox_provider = Arc::new(route.outbox_provider.clone());

    let mut fixture = TestFixture::new(outbox_provider.clone(), inbox_provider.clone());
    fixture.take_snapshots().await.unwrap();

    println!("--- SETUP: Creating epoch with messages and snapshot ---");
    let inbox = IVeaInboxArbToGnosis::new(route.inbox_address, inbox_provider.clone());
    let outbox = IVeaOutboxArbToGnosis::new(route.outbox_address, outbox_provider.clone());

    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

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

    advance_time(inbox_provider.as_ref(), epoch_period + 10).await;
    advance_time(outbox_provider.as_ref(), epoch_period + 10).await;

    let target_epoch = current_epoch;

    let gnosis_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let gnosis_timestamp = gnosis_block.header.timestamp;
    let target_timestamp = (target_epoch + 1) * epoch_period + 10;
    let advance_amount = target_timestamp.saturating_sub(gnosis_timestamp);
    if advance_amount > 0 {
        println!("Syncing Gnosis time (advancing {} seconds)", advance_amount);
        advance_time(outbox_provider.as_ref(), advance_amount).await;
    }

    println!("\n--- Setting up WETH for validator ---");
    let weth = IWETH::new(route.weth_address.unwrap(), outbox_provider.clone());
    let wallet_address = c.wallet.default_signer().address();

    let deposit = outbox.deposit().call().await.unwrap();
    weth.mintMock(wallet_address, deposit * U256::from(10)).send().await.unwrap().get_receipt().await.unwrap();
    weth.approve(route.outbox_address, deposit * U256::from(10)).send().await.unwrap().get_receipt().await.unwrap();
    println!("✓ WETH minted and approved for validator");

    println!("\n--- Starting Validator Components ---");
    let claim_handler = Arc::new(ClaimHandler::new(route.clone(), c.wallet.clone()));

    let event_listener = EventListener::new(route.outbox_provider.clone(), route.outbox_address);

    let challenge_detected = Arc::new(AtomicBool::new(false));
    let challenge_flag = challenge_detected.clone();

    let claim_handler_clone = claim_handler.clone();
    let watch_handle = tokio::spawn(async move {
        event_listener.watch_claims(move |event: ClaimEvent| {
            let handler = claim_handler_clone.clone();
            let flag = challenge_flag.clone();
            Box::pin(async move {
                println!("📡 Validator detected claim for epoch {} by {}", event.epoch, event.claimer);

                if let Ok(action) = handler.handle_claim_event(event.clone()).await {
                    match action {
                        vea_validator::claim_handler::ClaimAction::Challenge { epoch, incorrect_claim } => {
                            println!("⚔️  Validator decided to CHALLENGE epoch {}", epoch);

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

    println!("\n--- ATTACK: Malicious actor submits wrong claim on Gnosis ---");

    let wrong_root = FixedBytes::<32>::from([0x99; 32]);
    println!("Wrong root: {:?}", wrong_root);
    println!("Correct root: {:?}", correct_root);

    outbox.claim(U256::from(target_epoch), wrong_root)
        .send().await.unwrap()
        .get_receipt().await.unwrap();

    println!("✓ Malicious claim submitted on Gnosis");

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
async fn test_epoch_watcher_before_buffer_triggers_save_snapshot() {
    println!("\n==============================================");
    println!("EPOCH WATCHER TEST: Before Buffer → saveSnapshot");
    println!("==============================================\n");

    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let route = &routes[0];

    let inbox_provider = Arc::new(route.inbox_provider.clone());
    let outbox_provider = Arc::new(route.outbox_provider.clone());

    let mut fixture = TestFixture::new(outbox_provider.clone(), inbox_provider.clone());
    fixture.take_snapshots().await.unwrap();

    let inbox = IVeaInboxArbToEth::new(route.inbox_address, inbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    for i in 0..3 {
        let test_message = alloy::primitives::Bytes::from(vec![0xDE, 0xAD, 0xBE, 0xEF, i]);
        inbox.sendMessage(
            Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            test_message
        ).send().await.unwrap().get_receipt().await.unwrap();
    }

    let current_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();

    let initial_snapshot = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();
    assert_eq!(initial_snapshot, FixedBytes::<32>::ZERO, "Snapshot should not exist yet");

    println!("Current epoch: {}, messages sent, no snapshot yet", current_epoch);

    let claim_handler = Arc::new(ClaimHandler::new(route.clone(), c.wallet.clone()));

    let epoch_watcher = EpochWatcher::new(route.inbox_provider.clone());

    let snapshot_saved = Arc::new(AtomicBool::new(false));
    let snapshot_flag = snapshot_saved.clone();

    let handler_clone = claim_handler.clone();
    let watcher_handle = tokio::spawn(async move {
        epoch_watcher.watch_epochs(
            epoch_period,
            move |epoch| {
                let handler = handler_clone.clone();
                let flag = snapshot_flag.clone();
                Box::pin(async move {
                    println!("🕒 BEFORE_EPOCH_BUFFER triggered for epoch {}", epoch);
                    handler.handle_epoch_end(epoch).await.expect("saveSnapshot failed");
                    flag.store(true, Ordering::SeqCst);
                    Ok(())
                })
            },
            |_epoch| Box::pin(async move { Ok(()) }),
        ).await
    });

    tokio::time::sleep(Duration::from_millis(500)).await;

    let next_epoch_start = (current_epoch + 1) * epoch_period;
    let current_time = inbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap().header.timestamp;
    let time_to_before_buffer = next_epoch_start.saturating_sub(current_time).saturating_sub(299);

    println!("Advancing time by {} seconds to reach BEFORE_EPOCH_BUFFER", time_to_before_buffer);
    advance_time(inbox_provider.as_ref(), time_to_before_buffer).await;

    println!("Waiting for validator to call saveSnapshot...");
    let result = timeout(Duration::from_secs(15), async {
        while !snapshot_saved.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await;

    watcher_handle.abort();

    if result.is_err() {
        panic!("❌ VALIDATOR FAILED: Did not call saveSnapshot when BEFORE_EPOCH_BUFFER triggered");
    }

    let final_snapshot = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();
    assert_ne!(final_snapshot, FixedBytes::<32>::ZERO, "Snapshot should exist after saveSnapshot");

    println!("\n✅ EPOCH WATCHER TEST PASSED!");
    println!("The validator correctly called saveSnapshot when time reached BEFORE_EPOCH_BUFFER");

    fixture.revert_snapshots().await.unwrap();
}

#[tokio::test]
#[serial]
async fn test_epoch_watcher_after_buffer_triggers_claim() {
    println!("\n==============================================");
    println!("EPOCH WATCHER TEST: After Buffer → Claim");
    println!("==============================================\n");

    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let route = &routes[0];

    let inbox_provider = Arc::new(route.inbox_provider.clone());
    let outbox_provider = Arc::new(route.outbox_provider.clone());

    let mut fixture = TestFixture::new(outbox_provider.clone(), inbox_provider.clone());
    fixture.take_snapshots().await.unwrap();

    let inbox = IVeaInboxArbToEth::new(route.inbox_address, inbox_provider.clone());
    let outbox = IVeaOutboxArbToEth::new(route.outbox_address, outbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    for i in 0..3 {
        let test_message = alloy::primitives::Bytes::from(vec![0xCA, 0xFE, 0xBA, 0xBE, i]);
        inbox.sendMessage(
            Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            test_message
        ).send().await.unwrap().get_receipt().await.unwrap();
    }

    let current_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let snapshot = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();

    println!("Epoch {} snapshot saved: {:?}", current_epoch, snapshot);

    advance_time(inbox_provider.as_ref(), epoch_period + 70).await;

    let target_epoch = current_epoch;
    let dest_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let dest_timestamp = dest_block.header.timestamp;
    let target_timestamp = (target_epoch + 1) * epoch_period + 70;
    let advance_amount = target_timestamp.saturating_sub(dest_timestamp);
    if advance_amount > 0 {
        advance_time(outbox_provider.as_ref(), advance_amount).await;
    }

    let initial_claim_hash = outbox.claimHashes(U256::from(target_epoch)).call().await.unwrap();
    assert_eq!(initial_claim_hash, FixedBytes::<32>::ZERO, "Claim should not exist yet");

    let claim_handler = Arc::new(ClaimHandler::new(route.clone(), c.wallet.clone()));

    let epoch_watcher = EpochWatcher::new(route.inbox_provider.clone());

    let claim_submitted = Arc::new(AtomicBool::new(false));
    let claim_flag = claim_submitted.clone();

    let handler_clone = claim_handler.clone();
    let watcher_handle = tokio::spawn(async move {
        epoch_watcher.watch_epochs(
            epoch_period,
            |_epoch| Box::pin(async move { Ok(()) }),
            move |epoch| {
                let handler = handler_clone.clone();
                let flag = claim_flag.clone();
                Box::pin(async move {
                    println!("🕒 AFTER_EPOCH_BUFFER triggered for epoch {}", epoch);
                    if handler.handle_after_epoch_start(epoch).await.is_ok() {
                        flag.store(true, Ordering::SeqCst);
                    }
                    Ok(())
                })
            },
        ).await
    });

    tokio::time::sleep(Duration::from_millis(500)).await;

    println!("Waiting for validator to submit claim after AFTER_EPOCH_BUFFER...");
    let result = timeout(Duration::from_secs(15), async {
        while !claim_submitted.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await;

    watcher_handle.abort();

    if result.is_err() {
        panic!("❌ VALIDATOR FAILED: Did not submit claim when AFTER_EPOCH_BUFFER triggered");
    }

    let final_claim_hash = outbox.claimHashes(U256::from(target_epoch)).call().await.unwrap();
    assert_ne!(final_claim_hash, FixedBytes::<32>::ZERO, "Claim should exist after validator submits");

    println!("\n✅ EPOCH WATCHER CLAIM TEST PASSED!");
    println!("The validator correctly submitted claim when time reached AFTER_EPOCH_BUFFER");

    fixture.revert_snapshots().await.unwrap();
}

#[tokio::test]
#[serial]
async fn test_epoch_watcher_no_duplicate_save_snapshot() {
    println!("\n==============================================");
    println!("EPOCH WATCHER TEST: No Duplicate saveSnapshot");
    println!("==============================================\n");

    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let route = &routes[0];

    let inbox_provider = Arc::new(route.inbox_provider.clone());
    let outbox_provider = Arc::new(route.outbox_provider.clone());

    let mut fixture = TestFixture::new(outbox_provider.clone(), inbox_provider.clone());
    fixture.take_snapshots().await.unwrap();

    let inbox = IVeaInboxArbToEth::new(route.inbox_address, inbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    for i in 0..3 {
        let test_message = alloy::primitives::Bytes::from(vec![0xAB, 0xCD, 0xEF, i]);
        inbox.sendMessage(
            Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            test_message
        ).send().await.unwrap().get_receipt().await.unwrap();
    }

    let current_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();

    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let snapshot_before = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();
    println!("Snapshot already exists for epoch {}: {:?}", current_epoch, snapshot_before);

    let claim_handler = Arc::new(ClaimHandler::new(route.clone(), c.wallet.clone()));

    let epoch_watcher = EpochWatcher::new(route.inbox_provider.clone());

    let save_called = Arc::new(AtomicBool::new(false));
    let save_flag = save_called.clone();

    let handler_clone = claim_handler.clone();
    let watcher_handle = tokio::spawn(async move {
        epoch_watcher.watch_epochs(
            epoch_period,
            move |epoch| {
                let handler = handler_clone.clone();
                let flag = save_flag.clone();
                Box::pin(async move {
                    println!("🕒 BEFORE_EPOCH_BUFFER triggered for epoch {}", epoch);
                    handler.handle_epoch_end(epoch).await.expect("handle_epoch_end failed");
                    flag.store(true, Ordering::SeqCst);
                    Ok(())
                })
            },
            |_epoch| Box::pin(async move { Ok(()) }),
        ).await
    });

    tokio::time::sleep(Duration::from_millis(500)).await;

    let next_epoch_start = (current_epoch + 1) * epoch_period;
    let current_time = inbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap().header.timestamp;
    let time_to_before_buffer = next_epoch_start.saturating_sub(current_time).saturating_sub(299);

    println!("Advancing time to BEFORE_EPOCH_BUFFER...");
    advance_time(inbox_provider.as_ref(), time_to_before_buffer).await;

    tokio::time::sleep(Duration::from_secs(15)).await;

    watcher_handle.abort();

    let snapshot_after = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();
    assert_eq!(snapshot_before, snapshot_after, "Snapshot should not change - validator should skip if already exists");

    println!("\n✅ NO DUPLICATE SNAPSHOT TEST PASSED!");
    println!("The validator correctly skipped saveSnapshot when snapshot already existed");

    fixture.revert_snapshots().await.unwrap();
}

#[tokio::test]
#[serial]
async fn test_race_condition_claim_already_made() {
    println!("\n==============================================");
    println!("RACE CONDITION TEST: Claim Already Made");
    println!("==============================================\n");

    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let route = &routes[0];

    let inbox_provider = Arc::new(route.inbox_provider.clone());
    let outbox_provider = Arc::new(route.outbox_provider.clone());

    let mut fixture = TestFixture::new(outbox_provider.clone(), inbox_provider.clone());
    fixture.take_snapshots().await.unwrap();

    let inbox = IVeaInboxArbToEth::new(route.inbox_address, inbox_provider.clone());
    let outbox = IVeaOutboxArbToEth::new(route.outbox_address, outbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    for i in 0..3 {
        let test_message = alloy::primitives::Bytes::from(vec![0x11, 0x22, 0x33, i]);
        inbox.sendMessage(
            Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            test_message
        ).send().await.unwrap().get_receipt().await.unwrap();
    }

    let current_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let snapshot = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();

    println!("Epoch {} snapshot saved: {:?}", current_epoch, snapshot);

    advance_time(inbox_provider.as_ref(), epoch_period + 70).await;

    let target_epoch = current_epoch;
    let dest_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let dest_timestamp = dest_block.header.timestamp;
    let target_timestamp = (target_epoch + 1) * epoch_period + 70;
    let advance_amount = target_timestamp.saturating_sub(dest_timestamp);
    if advance_amount > 0 {
        advance_time(outbox_provider.as_ref(), advance_amount).await;
    }

    println!("Another validator submits claim first...");
    let deposit = outbox.deposit().call().await.unwrap();
    outbox.claim(U256::from(target_epoch), snapshot)
        .value(deposit)
        .send().await.unwrap()
        .get_receipt().await.unwrap();

    println!("✓ Claim already exists on-chain");

    let claim_handler = Arc::new(ClaimHandler::new(route.clone(), c.wallet.clone()));

    println!("Our validator attempts to claim (should fail gracefully)...");
    let result = claim_handler.handle_after_epoch_start(target_epoch).await;

    match result {
        Err(e) => {
            println!("✓ Validator got error (expected): {}", e);
            println!("✓ Validator did NOT panic - graceful handling");
        }
        Ok(_) => {
            println!("✓ Validator succeeded (contract might have allowed it, or detected existing claim)");
        }
    }

    println!("\n✅ RACE CONDITION TEST PASSED!");
    println!("Validator handles 'claim already made' without crashing");

    fixture.revert_snapshots().await.unwrap();
}

#[tokio::test]
#[serial]
async fn test_race_condition_challenge_already_made() {
    println!("\n==============================================");
    println!("RACE CONDITION TEST: Challenge Already Made");
    println!("==============================================\n");

    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let route = &routes[0];

    let inbox_provider = Arc::new(route.inbox_provider.clone());
    let outbox_provider = Arc::new(route.outbox_provider.clone());

    let mut fixture = TestFixture::new(outbox_provider.clone(), inbox_provider.clone());
    fixture.take_snapshots().await.unwrap();

    let inbox = IVeaInboxArbToEth::new(route.inbox_address, inbox_provider.clone());
    let outbox = IVeaOutboxArbToEth::new(route.outbox_address, outbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    for i in 0..3 {
        let test_message = alloy::primitives::Bytes::from(vec![0x77, 0x88, 0x99, i]);
        inbox.sendMessage(
            Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            test_message
        ).send().await.unwrap().get_receipt().await.unwrap();
    }

    let current_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let correct_root = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();

    println!("Epoch {} correct root: {:?}", current_epoch, correct_root);

    advance_time(inbox_provider.as_ref(), epoch_period + 70).await;
    advance_time(outbox_provider.as_ref(), epoch_period + 70).await;

    let target_epoch = current_epoch;
    let eth_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let eth_timestamp = eth_block.header.timestamp;
    let target_timestamp = (target_epoch + 1) * epoch_period + 70;
    let advance_amount = target_timestamp.saturating_sub(eth_timestamp);
    if advance_amount > 0 {
        advance_time(outbox_provider.as_ref(), advance_amount).await;
    }

    let event_listener = EventListener::new(route.outbox_provider.clone(), route.outbox_address);

    let claim_event_captured = Arc::new(tokio::sync::RwLock::new(None::<ClaimEvent>));
    let claim_event_flag = claim_event_captured.clone();

    let listener_handle = tokio::spawn(async move {
        event_listener.watch_claims(move |event: ClaimEvent| {
            let flag = claim_event_flag.clone();
            Box::pin(async move {
                let mut captured = flag.write().await;
                *captured = Some(event);
                Ok(())
            })
        }).await
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    println!("Malicious actor submits wrong claim...");
    let wrong_root = FixedBytes::<32>::from([0xEE; 32]);
    let deposit = outbox.deposit().call().await.unwrap();
    outbox.claim(U256::from(target_epoch), wrong_root)
        .value(deposit)
        .send().await.unwrap()
        .get_receipt().await.unwrap();

    println!("✓ Wrong claim submitted, waiting for event...");

    tokio::time::sleep(Duration::from_secs(2)).await;

    let claim_event = {
        let captured = claim_event_captured.read().await;
        captured.clone().expect("Should have captured claim event")
    };

    listener_handle.abort();

    println!("✓ Captured claim event: epoch {}, root {:?}", claim_event.epoch, claim_event.state_root);

    let claim_handler = Arc::new(ClaimHandler::new(route.clone(), c.wallet.clone()));

    println!("First validator challenges...");
    let claim_struct = vea_validator::claim_handler::make_claim(&claim_event);
    let first_result = claim_handler.challenge_claim(target_epoch, claim_struct.clone()).await;

    match first_result {
        Ok(_) => println!("✓ First challenge succeeded"),
        Err(e) => panic!("First challenge should succeed but failed: {}", e),
    }

    println!("Second validator attempts to challenge (race condition)...");
    let second_result = claim_handler.challenge_claim(target_epoch, claim_struct).await;

    match second_result {
        Err(e) => {
            let err_msg = e.to_string();
            println!("✓ Second challenge failed: {}", e);
            if err_msg.contains("already") || err_msg.contains("challenged") {
                println!("✓ Error indicates claim already challenged");
            }
            println!("✓ Validator did NOT panic - handled gracefully");
        }
        Ok(_) => {
            println!("⚠️  Second challenge succeeded (contract may allow it)");
            println!("✓ Validator did NOT panic");
        }
    }

    println!("\n✅ RACE CONDITION TEST PASSED!");
    println!("Validator handles duplicate challenges without crashing");

    fixture.revert_snapshots().await.unwrap();
}
