mod common;

use alloy::primitives::U256;
use serial_test::serial;
use std::sync::Arc;
use vea_validator::{
    contracts::{IVeaInboxArbToEth, IVeaOutboxArbToEth},
    config::ValidatorConfig,
    indexer::EventIndexer,
    tasks::dispatcher::TaskDispatcher,
    tasks,
};
use common::{restore_pristine, advance_time, send_messages};
use alloy::providers::Provider;

#[tokio::test]
#[serial]
async fn test_start_verification() {
    let c = ValidatorConfig::from_env().unwrap();
    let route = &c.build_routes()[0];
    let outbox_provider = Arc::new(route.outbox_provider.clone());
    restore_pristine().await;

    let inbox = IVeaInboxArbToEth::new(route.inbox_address, route.inbox_provider.clone());
    let outbox = IVeaOutboxArbToEth::new(route.outbox_address, outbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    send_messages(route).await;
    let epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let state_root = inbox.snapshots(U256::from(epoch)).call().await.unwrap();

    advance_time(epoch_period + 15 * 60 + 10).await;
    let ts = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap().header.timestamp;
    let target = (epoch + 1) * epoch_period + 15 * 60 + 10;
    if target > ts { advance_time(target - ts).await; }

    let deposit = outbox.deposit().call().await.unwrap();
    let claim_receipt = outbox.claim(U256::from(epoch), state_root).value(deposit)
        .send().await.unwrap().get_receipt().await.unwrap();
    let timestamp_claimed = outbox_provider.get_block_by_number(claim_receipt.block_number.unwrap().into())
        .await.unwrap().unwrap().header.timestamp as u32;

    advance_time(25 * 3600 + 10).await;

    let claimer = c.wallet.default_signer().address();
    tasks::start_verification::execute(route, epoch, state_root, claimer, timestamp_claimed).await.unwrap();

    let sig = alloy::primitives::keccak256("VerificationStarted(uint256)");
    let filter = alloy::rpc::types::Filter::new().address(route.outbox_address).event_signature(sig).from_block(0u64);
    let logs = outbox_provider.get_logs(&filter).await.unwrap();
    assert!(!logs.is_empty(), "VerificationStarted not emitted");
    assert_eq!(U256::from_be_bytes(logs[0].topics()[1].0).to::<u64>(), epoch);
}

#[tokio::test]
#[serial]
async fn test_verify_snapshot() {
    let c = ValidatorConfig::from_env().unwrap();
    let route = &c.build_routes()[0];
    let outbox_provider = Arc::new(route.outbox_provider.clone());
    restore_pristine().await;

    let inbox = IVeaInboxArbToEth::new(route.inbox_address, route.inbox_provider.clone());
    let outbox = IVeaOutboxArbToEth::new(route.outbox_address, outbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    send_messages(route).await;
    let epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let state_root = inbox.snapshots(U256::from(epoch)).call().await.unwrap();

    advance_time(epoch_period + 15 * 60 + 10).await;
    let ts = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap().header.timestamp;
    let target = (epoch + 1) * epoch_period + 15 * 60 + 10;
    if target > ts { advance_time(target - ts).await; }

    let deposit = outbox.deposit().call().await.unwrap();
    let claim_receipt = outbox.claim(U256::from(epoch), state_root).value(deposit)
        .send().await.unwrap().get_receipt().await.unwrap();
    let timestamp_claimed = outbox_provider.get_block_by_number(claim_receipt.block_number.unwrap().into())
        .await.unwrap().unwrap().header.timestamp as u32;

    advance_time(25 * 3600 + 10).await;

    let claimer = c.wallet.default_signer().address();
    tasks::start_verification::execute(route, epoch, state_root, claimer, timestamp_claimed).await.unwrap();

    let start_block = outbox_provider.get_block_number().await.unwrap();
    let start_ts = outbox_provider.get_block_by_number(start_block.into()).await.unwrap().unwrap().header.timestamp as u32;

    let min_challenge = outbox.minChallengePeriod().call().await.unwrap().to::<u64>();
    advance_time(min_challenge + 10).await;

    tasks::verify_snapshot::execute(
        route, epoch, state_root, claimer, timestamp_claimed, start_ts, start_block as u32
    ).await.unwrap();

    let sig = alloy::primitives::keccak256("Verified(uint256)");
    let filter = alloy::rpc::types::Filter::new().address(route.outbox_address).event_signature(sig).from_block(0u64);
    let logs = outbox_provider.get_logs(&filter).await.unwrap();
    assert!(!logs.is_empty(), "Verified not emitted");
}

#[tokio::test]
#[serial]
async fn test_full_happy_path_via_indexer() {
    let c = ValidatorConfig::from_env().unwrap();
    let route = &c.build_routes()[0];
    let outbox_provider = Arc::new(route.outbox_provider.clone());
    restore_pristine().await;

    let inbox = IVeaInboxArbToEth::new(route.inbox_address, route.inbox_provider.clone());
    let outbox = IVeaOutboxArbToEth::new(route.outbox_address, outbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    send_messages(route).await;
    let epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let state_root = inbox.snapshots(U256::from(epoch)).call().await.unwrap();

    advance_time(epoch_period + 15 * 60 + 10).await;
    let ts = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap().header.timestamp;
    let target = (epoch + 1) * epoch_period + 15 * 60 + 10;
    if target > ts { advance_time(target - ts).await; }

    let deposit = outbox.deposit().call().await.unwrap();
    outbox.claim(U256::from(epoch), state_root).value(deposit).send().await.unwrap().get_receipt().await.unwrap();

    advance_time(15 * 60 + 10).await;

    let test_dir = tempfile::tempdir().unwrap();
    let schedule_path = test_dir.path().join("schedule.json");
    let indexer = EventIndexer::new(route.clone(), schedule_path.clone(), test_dir.path().join("claims.json"));
    let dispatcher = TaskDispatcher::new(c.clone(), route.clone(), schedule_path);

    indexer.scan_once().await;
    dispatcher.process_pending().await;

    advance_time(25 * 3600 + 10).await;
    dispatcher.process_pending().await;

    let sig = alloy::primitives::keccak256("VerificationStarted(uint256)");
    let filter = alloy::rpc::types::Filter::new().address(route.outbox_address).event_signature(sig).from_block(0u64);
    let logs = outbox_provider.get_logs(&filter).await.unwrap();
    assert!(!logs.is_empty(), "Indexer/Dispatcher did not start verification");

    advance_time(15 * 60 + 10).await;
    indexer.scan_once().await;

    let min_challenge = outbox.minChallengePeriod().call().await.unwrap().to::<u64>();
    advance_time(min_challenge + 10).await;
    dispatcher.process_pending().await;

    let sig = alloy::primitives::keccak256("Verified(uint256)");
    let filter = alloy::rpc::types::Filter::new().address(route.outbox_address).event_signature(sig).from_block(0u64);
    let logs = outbox_provider.get_logs(&filter).await.unwrap();
    assert!(!logs.is_empty(), "Indexer/Dispatcher did not verify snapshot");
}
