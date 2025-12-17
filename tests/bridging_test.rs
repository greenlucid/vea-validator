mod common;

use alloy::primitives::{Address, FixedBytes, U256};
use serial_test::serial;
use std::sync::{Arc, Mutex};
use vea_validator::{
    contracts::{IVeaInboxArbToEth, IVeaOutboxArbToEth, IVeaInboxArbToGnosis, IVeaOutboxArbToGnosis, IWETH},
    config::ValidatorConfig,
    indexer::EventIndexer,
    tasks::{dispatcher::TaskDispatcher, TaskStore, ClaimStore},
    startup::ensure_weth_approval,
};
use std::str::FromStr;
use common::{restore_pristine, advance_time, send_messages};
use alloy::providers::Provider;

#[tokio::test]
#[serial]
async fn test_send_snapshot_after_challenge() {
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

    advance_time(epoch_period + 15 * 60 + 10).await;
    let ts = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap().header.timestamp;
    let target = (epoch + 1) * epoch_period + 15 * 60 + 10;
    if target > ts { advance_time(target - ts).await; }

    let bad_root = FixedBytes::<32>::from([0xBA; 32]);
    let deposit = outbox.deposit().call().await.unwrap();
    outbox.claim(U256::from(epoch), bad_root).value(deposit).send().await.unwrap().get_receipt().await.unwrap();

    advance_time(15 * 60 + 10).await;

    let test_dir = tempfile::tempdir().unwrap();
    let schedule_path = test_dir.path().join("schedule.json");
    let claims_path = test_dir.path().join("claims.json");
    let task_store = Arc::new(Mutex::new(TaskStore::new(&schedule_path)));
    let claim_store = Arc::new(Mutex::new(ClaimStore::new(&claims_path)));
    let wallet_address = c.wallet.default_signer().address();
    let indexer = EventIndexer::new(route.clone(), wallet_address, task_store.clone(), claim_store.clone());
    indexer.initialize().await;
    task_store.lock().unwrap().set_on_sync(true);
    let dispatcher = TaskDispatcher::new(c.clone(), route.clone(), task_store, claim_store);

    indexer.scan_once().await;
    dispatcher.process_pending().await;
    dispatcher.process_pending().await;

    advance_time(15 * 60 + 10).await;
    indexer.scan_once().await;
    dispatcher.process_pending().await;

    let sig = alloy::primitives::keccak256("SnapshotSent(uint256,bytes32)");
    let filter = alloy::rpc::types::Filter::new().address(route.inbox_address).event_signature(sig).from_block(0u64);
    let logs = route.inbox_provider.get_logs(&filter).await.unwrap();
    assert!(!logs.is_empty(), "SnapshotSent not emitted");
}

#[tokio::test]
#[serial]
async fn test_send_snapshot_on_challenged_event() {
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
    let claims_path = test_dir.path().join("claims.json");
    let task_store = Arc::new(Mutex::new(TaskStore::new(&schedule_path)));
    let claim_store = Arc::new(Mutex::new(ClaimStore::new(&claims_path)));
    let wallet_address = c.wallet.default_signer().address();
    let indexer = EventIndexer::new(route.clone(), wallet_address, task_store.clone(), claim_store.clone());
    indexer.initialize().await;
    task_store.lock().unwrap().set_on_sync(true);
    let dispatcher = TaskDispatcher::new(c.clone(), route.clone(), task_store, claim_store);

    indexer.scan_once().await;

    let claim = vea_validator::contracts::Claim {
        stateRoot: state_root,
        claimer: c.wallet.default_signer().address(),
        timestampClaimed: outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap().header.timestamp as u32 - 15 * 60 - 10,
        timestampVerification: 0,
        blocknumberVerification: 0,
        honest: vea_validator::contracts::Party::None,
        challenger: Address::ZERO,
    };
    outbox.challenge(U256::from(epoch), claim).value(deposit).send().await.unwrap().get_receipt().await.unwrap();

    advance_time(15 * 60 + 10).await;
    indexer.scan_once().await;
    dispatcher.process_pending().await;

    let sig = alloy::primitives::keccak256("SnapshotSent(uint256,bytes32)");
    let filter = alloy::rpc::types::Filter::new().address(route.inbox_address).event_signature(sig).from_block(0u64);
    let logs = route.inbox_provider.get_logs(&filter).await.unwrap();
    assert!(!logs.is_empty(), "Indexer did not send snapshot on Challenged event");
}

#[tokio::test]
#[serial]
async fn test_execute_relay() {
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

    let claimer = c.wallet.default_signer().address();

    let claim = vea_validator::contracts::Claim {
        stateRoot: state_root,
        claimer,
        timestampClaimed: timestamp_claimed,
        timestampVerification: 0,
        blocknumberVerification: 0,
        honest: vea_validator::contracts::Party::None,
        challenger: Address::ZERO,
    };
    outbox.challenge(U256::from(epoch), claim).value(deposit).send().await.unwrap().get_receipt().await.unwrap();
    let balance_after_challenge = outbox_provider.get_balance(c.wallet.default_signer().address()).await.unwrap();

    advance_time(15 * 60 + 10).await;

    let test_dir = tempfile::tempdir().unwrap();
    let schedule_path = test_dir.path().join("schedule.json");
    let claims_path = test_dir.path().join("claims.json");
    let task_store = Arc::new(Mutex::new(TaskStore::new(&schedule_path)));
    let claim_store = Arc::new(Mutex::new(ClaimStore::new(&claims_path)));
    let wallet_address = c.wallet.default_signer().address();
    let indexer = EventIndexer::new(route.clone(), wallet_address, task_store.clone(), claim_store.clone());
    indexer.initialize().await;
    task_store.lock().unwrap().set_on_sync(true);
    let dispatcher = TaskDispatcher::new(c.clone(), route.clone(), task_store, claim_store.clone());

    indexer.scan_once().await;
    dispatcher.process_pending().await;

    advance_time(15 * 60 + 10).await;
    indexer.scan_once().await;

    let relay_delay = 7 * 24 * 3600 + 3600;
    advance_time(relay_delay + 10).await;
    dispatcher.process_pending().await;

    let sig = alloy::primitives::keccak256("Verified(uint256)");
    let filter = alloy::rpc::types::Filter::new().address(route.outbox_address).event_signature(sig).from_block(0u64);
    let logs = outbox_provider.get_logs(&filter).await.unwrap();
    assert!(!logs.is_empty(), "Verified not emitted - resolveDisputedClaim failed");

    advance_time(15 * 60 + 10).await;
    indexer.scan_once().await;
    dispatcher.process_pending().await;

    let balance_after_withdraw = outbox_provider.get_balance(wallet_address).await.unwrap();
    assert!(balance_after_withdraw > balance_after_challenge, "Deposit was not returned to claimer (honest) after disputed verification");

    assert!(std::panic::catch_unwind(|| claim_store.lock().unwrap().get(epoch)).is_err(), "Claim should be removed after withdraw");
}

#[tokio::test]
#[serial]
async fn test_send_snapshot_gnosis() {
    let c = ValidatorConfig::from_env().unwrap();
    let route = &c.build_routes()[1];
    let wallet_address = c.wallet.default_signer().address();
    let outbox_provider = Arc::new(route.outbox_provider.clone());
    restore_pristine().await;

    let weth = IWETH::new(route.weth_address.unwrap(), outbox_provider.clone());
    weth.deposit().value(U256::from(10u64).pow(U256::from(19))).send().await.unwrap().get_receipt().await.unwrap();
    ensure_weth_approval(&c, route.outbox_provider.clone(), wallet_address).await.unwrap();

    let inbox = IVeaInboxArbToGnosis::new(route.inbox_address, route.inbox_provider.clone());
    let outbox = IVeaOutboxArbToGnosis::new(route.outbox_address, outbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    for i in 0..3u8 {
        let msg = alloy::primitives::Bytes::from(vec![0xAA, 0xBB, i]);
        inbox.sendMessage(Address::from_str("0x0000000000000000000000000000000000000001").unwrap(), msg)
            .send().await.unwrap().get_receipt().await.unwrap();
    }

    let epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();

    advance_time(epoch_period + 15 * 60 + 10).await;
    let ts = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap().header.timestamp;
    let target = (epoch + 1) * epoch_period + 15 * 60 + 10;
    if target > ts { advance_time(target - ts).await; }

    let bad_root = FixedBytes::<32>::from([0xBA; 32]);
    outbox.claim(U256::from(epoch), bad_root).send().await.unwrap().get_receipt().await.unwrap();

    advance_time(15 * 60 + 10).await;

    let test_dir = tempfile::tempdir().unwrap();
    let schedule_path = test_dir.path().join("schedule.json");
    let claims_path = test_dir.path().join("claims.json");
    let task_store = Arc::new(Mutex::new(TaskStore::new(&schedule_path)));
    let claim_store = Arc::new(Mutex::new(ClaimStore::new(&claims_path)));
    let indexer = EventIndexer::new(route.clone(), wallet_address, task_store.clone(), claim_store.clone());
    indexer.initialize().await;
    task_store.lock().unwrap().set_on_sync(true);
    let dispatcher = TaskDispatcher::new(c.clone(), route.clone(), task_store, claim_store);

    indexer.scan_once().await;
    dispatcher.process_pending().await;
    dispatcher.process_pending().await;

    advance_time(15 * 60 + 10).await;
    indexer.scan_once().await;
    dispatcher.process_pending().await;

    let sig = alloy::primitives::keccak256("SnapshotSent(uint256,bytes32)");
    let filter = alloy::rpc::types::Filter::new().address(route.inbox_address).event_signature(sig).from_block(0u64);
    let logs = route.inbox_provider.get_logs(&filter).await.unwrap();
    assert!(!logs.is_empty(), "SnapshotSent not emitted on Gnosis route");
}

#[tokio::test]
#[serial]
async fn test_execute_relay_skips_spent() {
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

    let claimer = c.wallet.default_signer().address();
    let claim = vea_validator::contracts::Claim {
        stateRoot: state_root,
        claimer,
        timestampClaimed: timestamp_claimed,
        timestampVerification: 0,
        blocknumberVerification: 0,
        honest: vea_validator::contracts::Party::None,
        challenger: Address::ZERO,
    };
    outbox.challenge(U256::from(epoch), claim).value(deposit).send().await.unwrap().get_receipt().await.unwrap();

    advance_time(15 * 60 + 10).await;

    let test_dir = tempfile::tempdir().unwrap();
    let schedule_path = test_dir.path().join("schedule.json");
    let claims_path = test_dir.path().join("claims.json");
    let task_store = Arc::new(Mutex::new(TaskStore::new(&schedule_path)));
    let claim_store = Arc::new(Mutex::new(ClaimStore::new(&claims_path)));
    let wallet_address = c.wallet.default_signer().address();
    let indexer = EventIndexer::new(route.clone(), wallet_address, task_store.clone(), claim_store.clone());
    indexer.initialize().await;
    task_store.lock().unwrap().set_on_sync(true);
    let dispatcher = TaskDispatcher::new(c.clone(), route.clone(), task_store.clone(), claim_store.clone());

    indexer.scan_once().await;
    advance_time(15 * 60 + 10).await;
    indexer.scan_once().await;

    let relay_delay = 7 * 24 * 3600 + 3600;
    advance_time(relay_delay + 10).await;
    dispatcher.process_pending().await;

    let dispatcher2 = TaskDispatcher::new(c.clone(), route.clone(), task_store, claim_store);
    dispatcher2.process_pending().await;
}

#[tokio::test]
#[serial]
async fn test_challenger_wins_bad_claim() {
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
    inbox.snapshots(U256::from(epoch)).call().await.unwrap();

    advance_time(epoch_period + 15 * 60 + 10).await;
    let ts = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap().header.timestamp;
    let target = (epoch + 1) * epoch_period + 15 * 60 + 10;
    if target > ts { advance_time(target - ts).await; }

    let bad_root = alloy::primitives::FixedBytes::<32>::from([0xBA; 32]);
    let deposit = outbox.deposit().call().await.unwrap();
    let claim_receipt = outbox.claim(U256::from(epoch), bad_root).value(deposit)
        .send().await.unwrap().get_receipt().await.unwrap();
    let timestamp_claimed = outbox_provider.get_block_by_number(claim_receipt.block_number.unwrap().into())
        .await.unwrap().unwrap().header.timestamp as u32;

    advance_time(15 * 60 + 10).await;

    let test_dir = tempfile::tempdir().unwrap();
    let schedule_path = test_dir.path().join("schedule.json");
    let claims_path = test_dir.path().join("claims.json");
    let task_store = Arc::new(Mutex::new(TaskStore::new(&schedule_path)));
    let claim_store = Arc::new(Mutex::new(ClaimStore::new(&claims_path)));
    let wallet_address = c.wallet.default_signer().address();
    let indexer = EventIndexer::new(route.clone(), wallet_address, task_store.clone(), claim_store.clone());
    indexer.initialize().await;
    task_store.lock().unwrap().set_on_sync(true);
    let dispatcher = TaskDispatcher::new(c.clone(), route.clone(), task_store, claim_store);

    indexer.scan_once().await;
    dispatcher.process_pending().await;
    dispatcher.process_pending().await;

    let challenged_sig = alloy::primitives::keccak256("Challenged(uint256,address)");
    let filter = alloy::rpc::types::Filter::new().address(route.outbox_address).event_signature(challenged_sig).from_block(0u64);
    let logs = outbox_provider.get_logs(&filter).await.unwrap();
    assert!(!logs.is_empty(), "Validator did not challenge bad claim");

    advance_time(15 * 60 + 10).await;
    indexer.scan_once().await;
    dispatcher.process_pending().await;

    advance_time(15 * 60 + 10).await;
    indexer.scan_once().await;

    let relay_delay = 7 * 24 * 3600 + 3600;
    advance_time(relay_delay + 10).await;
    dispatcher.process_pending().await;

    let claim_after = vea_validator::contracts::Claim {
        stateRoot: bad_root,
        claimer: c.wallet.default_signer().address(),
        timestampClaimed: timestamp_claimed,
        timestampVerification: 0,
        blocknumberVerification: 0,
        honest: vea_validator::contracts::Party::Challenger,
        challenger: c.wallet.default_signer().address(),
    };
    let claim_hash = outbox.claimHashes(U256::from(epoch)).call().await.unwrap();
    let expected_hash = outbox.hashClaim(claim_after).call().await.unwrap();
    assert_eq!(claim_hash, expected_hash, "Challenger should have won");
}
