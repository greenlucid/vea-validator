mod common;

use alloy::primitives::{FixedBytes, U256};
use serial_test::serial;
use std::sync::{Arc, Mutex};
use tokio::time::{timeout, Duration};
use vea_validator::{
    contracts::{IVeaInboxArbToEth, IVeaOutboxArbToEth},
    config::ValidatorConfig,
    epoch_watcher::EpochWatcher,
    tasks::{TaskStore, ClaimStore},
};
use common::{restore_pristine, advance_time, send_messages};
use alloy::providers::Provider;

#[tokio::test]
#[serial]
async fn test_save_snapshot() {
    let c = ValidatorConfig::from_env().unwrap();
    let route = &c.build_routes()[0];
    let inbox_provider = Arc::new(route.inbox_provider.clone());
    restore_pristine().await;

    let inbox = IVeaInboxArbToEth::new(route.inbox_address, inbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    send_messages(route).await;
    let epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    assert_eq!(inbox.snapshots(U256::from(epoch)).call().await.unwrap(), FixedBytes::<32>::ZERO);

    let test_dir = tempfile::tempdir().unwrap();
    let schedule_path = test_dir.path().join("schedule.json");
    let claims_path = test_dir.path().join("claims.json");
    let task_store = Arc::new(Mutex::new(TaskStore::new(&schedule_path)));
    let claim_store = Arc::new(Mutex::new(ClaimStore::new(&claims_path)));
    task_store.lock().unwrap().set_on_sync(true);
    let watcher = EpochWatcher::new(route.clone(), true, claim_store, task_store);
    let handle = tokio::spawn(async move { watcher.watch_epochs(epoch_period).await });
    tokio::time::sleep(Duration::from_millis(500)).await;

    let next_epoch = (epoch + 1) * epoch_period;
    let now = inbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap().header.timestamp;
    advance_time(next_epoch.saturating_sub(now).saturating_sub(59)).await;

    timeout(Duration::from_secs(30), async {
        loop {
            if inbox.snapshots(U256::from(epoch)).call().await.unwrap() != FixedBytes::<32>::ZERO { break; }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await.expect("Validator did not save snapshot");
    handle.abort();
}

#[tokio::test]
#[serial]
async fn test_claim() {
    let c = ValidatorConfig::from_env().unwrap();
    let route = &c.build_routes()[0];
    let inbox_provider = Arc::new(route.inbox_provider.clone());
    let outbox_provider = Arc::new(route.outbox_provider.clone());
    restore_pristine().await;

    let inbox = IVeaInboxArbToEth::new(route.inbox_address, inbox_provider.clone());
    let outbox = IVeaOutboxArbToEth::new(route.outbox_address, outbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    send_messages(route).await;
    let epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();

    advance_time(epoch_period + 15 * 60 + 10).await;
    let ts = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap().header.timestamp;
    let target = (epoch + 1) * epoch_period + 15 * 60 + 10;
    if target > ts { advance_time(target - ts).await; }

    assert_eq!(outbox.claimHashes(U256::from(epoch)).call().await.unwrap(), FixedBytes::<32>::ZERO);

    let test_dir = tempfile::tempdir().unwrap();
    let schedule_path = test_dir.path().join("schedule.json");
    let claims_path = test_dir.path().join("claims.json");
    let task_store = Arc::new(Mutex::new(TaskStore::new(&schedule_path)));
    let claim_store = Arc::new(Mutex::new(ClaimStore::new(&claims_path)));
    task_store.lock().unwrap().set_on_sync(true);
    let watcher = EpochWatcher::new(route.clone(), true, claim_store, task_store);
    let handle = tokio::spawn(async move { watcher.watch_epochs(epoch_period).await });
    tokio::time::sleep(Duration::from_millis(500)).await;

    timeout(Duration::from_secs(15), async {
        loop {
            if outbox.claimHashes(U256::from(epoch)).call().await.unwrap() != FixedBytes::<32>::ZERO { break; }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await.expect("Validator did not submit claim");
    handle.abort();
}

#[tokio::test]
#[serial]
async fn test_no_duplicate_snapshot() {
    let c = ValidatorConfig::from_env().unwrap();
    let route = &c.build_routes()[0];
    let inbox_provider = Arc::new(route.inbox_provider.clone());
    restore_pristine().await;

    let inbox = IVeaInboxArbToEth::new(route.inbox_address, inbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    send_messages(route).await;
    let epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let snapshot_before = inbox.snapshots(U256::from(epoch)).call().await.unwrap();

    let test_dir = tempfile::tempdir().unwrap();
    let schedule_path = test_dir.path().join("schedule.json");
    let claims_path = test_dir.path().join("claims.json");
    let task_store = Arc::new(Mutex::new(TaskStore::new(&schedule_path)));
    let claim_store = Arc::new(Mutex::new(ClaimStore::new(&claims_path)));
    task_store.lock().unwrap().set_on_sync(true);
    let watcher = EpochWatcher::new(route.clone(), true, claim_store, task_store);
    let handle = tokio::spawn(async move { watcher.watch_epochs(epoch_period).await });
    tokio::time::sleep(Duration::from_millis(500)).await;

    let next_epoch = (epoch + 1) * epoch_period;
    let now = inbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap().header.timestamp;
    advance_time(next_epoch.saturating_sub(now).saturating_sub(59)).await;

    tokio::time::sleep(Duration::from_secs(2)).await;
    handle.abort();

    assert_eq!(inbox.snapshots(U256::from(epoch)).call().await.unwrap(), snapshot_before);
}

#[tokio::test]
#[serial]
async fn test_claim_race_condition() {
    let c = ValidatorConfig::from_env().unwrap();
    let route = &c.build_routes()[0];
    let inbox_provider = Arc::new(route.inbox_provider.clone());
    let outbox_provider = Arc::new(route.outbox_provider.clone());
    restore_pristine().await;

    let inbox = IVeaInboxArbToEth::new(route.inbox_address, inbox_provider.clone());
    let outbox = IVeaOutboxArbToEth::new(route.outbox_address, outbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    send_messages(route).await;
    let epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let snapshot = inbox.snapshots(U256::from(epoch)).call().await.unwrap();

    advance_time(epoch_period + 15 * 60 + 10).await;
    let ts = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap().header.timestamp;
    let target = (epoch + 1) * epoch_period + 15 * 60 + 10;
    if target > ts { advance_time(target - ts).await; }

    let deposit = outbox.deposit().call().await.unwrap();
    outbox.claim(U256::from(epoch), snapshot).value(deposit).send().await.unwrap().get_receipt().await.unwrap();

    let test_dir = tempfile::tempdir().unwrap();
    let claim_store = Arc::new(Mutex::new(ClaimStore::new(test_dir.path().join("claims.json"))));
    let ts = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap().header.timestamp;
    let result = vea_validator::tasks::claim::execute(route, epoch, &claim_store, ts).await;
    assert!(result.is_ok(), "Validator should handle existing claim gracefully");
}
