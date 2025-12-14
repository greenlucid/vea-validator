mod common;

use alloy::primitives::{Address, FixedBytes, U256};
use serial_test::serial;
use std::str::FromStr;
use std::sync::Arc;
use tokio::time::{timeout, Duration};
use vea_validator::{
    contracts::{IVeaInboxArbToEth, IVeaOutboxArbToEth},
    config::ValidatorConfig,
    epoch_watcher::EpochWatcher,
    tasks,
};
use common::{restore_pristine, advance_time};
use alloy::providers::Provider;

#[tokio::test]
#[serial]
async fn test_epoch_watcher_saves_snapshot_before_epoch_ends() {
    println!("\n==============================================");
    println!("EPOCH WATCHER TEST: Saves Snapshot Before Epoch Ends");
    println!("==============================================\n");

    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let route = &routes[0];

    let inbox_provider = Arc::new(route.inbox_provider.clone());

    restore_pristine().await;

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

    let epoch_watcher = EpochWatcher::new(
        route.inbox_provider.clone(),
        route.inbox_address,
        route.outbox_provider.clone(),
        route.outbox_address,
        "TEST",
        true,
    );
    let watcher_handle = tokio::spawn(async move {
        epoch_watcher.watch_epochs(epoch_period).await
    });

    tokio::time::sleep(Duration::from_millis(500)).await;

    let next_epoch_start = (current_epoch + 1) * epoch_period;
    let current_time = inbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap().header.timestamp;
    let time_to_before_buffer = next_epoch_start.saturating_sub(current_time).saturating_sub(59);

    println!("Advancing time by {} seconds to reach BEFORE_EPOCH_BUFFER", time_to_before_buffer);
    advance_time(time_to_before_buffer).await;

    println!("Waiting for validator to call saveSnapshot...");
    let result = timeout(Duration::from_secs(30), async {
        loop {
            let snapshot = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();
            if snapshot != FixedBytes::<32>::ZERO {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await;

    watcher_handle.abort();

    if result.is_err() {
        panic!("VALIDATOR FAILED: Did not call saveSnapshot when BEFORE_EPOCH_BUFFER triggered");
    }

    println!("\nEPOCH WATCHER TEST PASSED!");
    println!("The validator correctly called saveSnapshot when time reached BEFORE_EPOCH_BUFFER");
}

#[tokio::test]
#[serial]
async fn test_epoch_watcher_after_epoch_verifies_claim() {
    println!("\n==============================================");
    println!("EPOCH WATCHER TEST: Verifies Claim After Epoch");
    println!("==============================================\n");

    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let route = &routes[0];

    let inbox_provider = Arc::new(route.inbox_provider.clone());
    let outbox_provider = Arc::new(route.outbox_provider.clone());

    restore_pristine().await;

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

    let target_epoch = current_epoch;

    let after_epoch_buffer = 15 * 60;
    advance_time(epoch_period + after_epoch_buffer + 10).await;

    let dest_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let dest_timestamp = dest_block.header.timestamp;
    let target_timestamp = (target_epoch + 1) * epoch_period + after_epoch_buffer + 10;
    let advance_amount = target_timestamp.saturating_sub(dest_timestamp);
    if advance_amount > 0 {
        advance_time(advance_amount).await;
    }

    let initial_claim_hash = outbox.claimHashes(U256::from(target_epoch)).call().await.unwrap();
    assert_eq!(initial_claim_hash, FixedBytes::<32>::ZERO, "Claim should not exist yet");

    let epoch_watcher = EpochWatcher::new(
        route.inbox_provider.clone(),
        route.inbox_address,
        route.outbox_provider.clone(),
        route.outbox_address,
        "TEST",
        true,
    );
    let watcher_handle = tokio::spawn(async move {
        epoch_watcher.watch_epochs(epoch_period).await
    });

    tokio::time::sleep(Duration::from_millis(500)).await;

    println!("Waiting for validator to submit claim after AFTER_EPOCH_BUFFER (15min)...");
    let result = timeout(Duration::from_secs(15), async {
        loop {
            let claim_hash = outbox.claimHashes(U256::from(target_epoch)).call().await.unwrap();
            if claim_hash != FixedBytes::<32>::ZERO {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await;

    watcher_handle.abort();

    if result.is_err() {
        panic!("VALIDATOR FAILED: Did not submit claim when AFTER_EPOCH_BUFFER triggered");
    }

    println!("\nEPOCH WATCHER CLAIM TEST PASSED!");
    println!("The validator correctly submitted claim when time reached AFTER_EPOCH_BUFFER");
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

    restore_pristine().await;

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

    let epoch_watcher = EpochWatcher::new(
        route.inbox_provider.clone(),
        route.inbox_address,
        route.outbox_provider.clone(),
        route.outbox_address,
        "TEST",
        true,
    );
    let watcher_handle = tokio::spawn(async move {
        epoch_watcher.watch_epochs(epoch_period).await
    });

    tokio::time::sleep(Duration::from_millis(500)).await;

    let next_epoch_start = (current_epoch + 1) * epoch_period;
    let current_time = inbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap().header.timestamp;
    let time_to_before_buffer = next_epoch_start.saturating_sub(current_time).saturating_sub(59);

    println!("Advancing time to BEFORE_EPOCH_BUFFER...");
    advance_time(time_to_before_buffer).await;

    tokio::time::sleep(Duration::from_secs(15)).await;

    watcher_handle.abort();

    let snapshot_after = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();
    assert_eq!(snapshot_before, snapshot_after, "Snapshot should not change - validator should skip if already exists");

    println!("\nNO DUPLICATE SNAPSHOT TEST PASSED!");
    println!("The validator correctly skipped saveSnapshot when snapshot already existed");
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

    restore_pristine().await;

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

    let target_epoch = current_epoch;

    let after_epoch_buffer = 15 * 60;
    advance_time(epoch_period + after_epoch_buffer + 10).await;

    let dest_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let dest_timestamp = dest_block.header.timestamp;
    let target_timestamp = (target_epoch + 1) * epoch_period + after_epoch_buffer + 10;
    let advance_amount = target_timestamp.saturating_sub(dest_timestamp);
    if advance_amount > 0 {
        advance_time(advance_amount).await;
    }

    println!("Another validator submits claim first...");
    let deposit = outbox.deposit().call().await.unwrap();
    outbox.claim(U256::from(target_epoch), snapshot)
        .value(deposit)
        .send().await.unwrap()
        .get_receipt().await.unwrap();

    println!("Claim already exists on-chain");

    println!("Our validator attempts to claim (should fail gracefully)...");
    let result = tasks::claim::execute(
        route.inbox_provider.clone(),
        route.inbox_address,
        route.outbox_provider.clone(),
        route.outbox_address,
        target_epoch,
        "TEST",
    ).await;

    match result {
        Err(e) => {
            println!("Validator got error (expected): {}", e);
            println!("Validator did NOT panic - graceful handling");
        }
        Ok(_) => {
            println!("Validator succeeded (detected existing claim)");
        }
    }

    println!("\nRACE CONDITION TEST PASSED!");
    println!("Validator handles 'claim already made' without crashing");
}
