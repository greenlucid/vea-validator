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
use common::{restore_pristine, advance_time, send_messages};
use alloy::providers::Provider;

#[tokio::test]
#[serial]
async fn test_challenge_bad_claim() {
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

    let wrong_root = FixedBytes::<32>::from([0xDE, 0xAD, 0xBE, 0xEF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let deposit = outbox.deposit().call().await.unwrap();
    outbox.claim(U256::from(epoch), wrong_root).value(deposit).send().await.unwrap().get_receipt().await.unwrap();

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

    let sig = alloy::primitives::keccak256("Challenged(uint256,address)");
    let filter = alloy::rpc::types::Filter::new().address(route.outbox_address).event_signature(sig).from_block(0u64);
    let logs = outbox_provider.get_logs(&filter).await.unwrap();
    assert!(!logs.is_empty(), "Validator did not challenge");
    assert_eq!(U256::from_be_bytes(logs[0].topics()[1].0).to::<u64>(), epoch);
}

#[tokio::test]
#[serial]
async fn test_challenge_bad_claim_gnosis() {
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

    let wrong_root = FixedBytes::<32>::from([0xDE, 0xAD, 0xBE, 0xEF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    outbox.claim(U256::from(epoch), wrong_root).send().await.unwrap().get_receipt().await.unwrap();

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

    let sig = alloy::primitives::keccak256("Challenged(uint256,address)");
    let filter = alloy::rpc::types::Filter::new().address(route.outbox_address).event_signature(sig).from_block(0u64);
    let logs = outbox_provider.get_logs(&filter).await.unwrap();
    assert!(!logs.is_empty(), "Validator did not challenge on Gnosis");
    assert_eq!(U256::from_be_bytes(logs[0].topics()[1].0).to::<u64>(), epoch);
}

#[tokio::test]
#[serial]
async fn test_weth_approval_set_on_startup() {
    let c = ValidatorConfig::from_env().unwrap();
    let route = &c.build_routes()[1];
    let outbox_provider = Arc::new(route.outbox_provider.clone());
    restore_pristine().await;

    let weth = IWETH::new(route.weth_address.unwrap(), outbox_provider.clone());
    let wallet_address = c.wallet.default_signer().address();

    if weth.allowance(wallet_address, route.outbox_address).call().await.unwrap() > U256::ZERO {
        weth.approve(route.outbox_address, U256::ZERO).from(wallet_address).send().await.unwrap().get_receipt().await.unwrap();
    }
    assert_eq!(weth.allowance(wallet_address, route.outbox_address).call().await.unwrap(), U256::ZERO);

    ensure_weth_approval(&c, route.outbox_provider.clone(), wallet_address).await.unwrap();
    assert_eq!(weth.allowance(wallet_address, route.outbox_address).call().await.unwrap(), U256::MAX);
}

#[tokio::test]
#[serial]
async fn test_weth_approval_skipped_if_exists() {
    let c = ValidatorConfig::from_env().unwrap();
    let route = &c.build_routes()[1];
    let outbox_provider = Arc::new(route.outbox_provider.clone());
    restore_pristine().await;

    let weth = IWETH::new(route.weth_address.unwrap(), outbox_provider.clone());
    let wallet_address = c.wallet.default_signer().address();

    let manual = U256::from(1000000000u64);
    weth.approve(route.outbox_address, manual).from(wallet_address).send().await.unwrap().get_receipt().await.unwrap();

    ensure_weth_approval(&c, route.outbox_provider.clone(), wallet_address).await.unwrap();
    assert_eq!(weth.allowance(wallet_address, route.outbox_address).call().await.unwrap(), manual);
}

use std::str::FromStr;
use vea_validator::tasks::TaskKind;

#[tokio::test]
#[serial]
async fn test_start_verification_drops_task_when_claim_challenged() {
    let c = ValidatorConfig::from_env().unwrap();
    let route = &c.build_routes()[0];
    let outbox_provider = Arc::new(route.outbox_provider.clone());
    restore_pristine().await;

    let inbox = IVeaInboxArbToEth::new(route.inbox_address, route.inbox_provider.clone());
    let outbox = IVeaOutboxArbToEth::new(route.outbox_address, outbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();
    let deposit = outbox.deposit().call().await.unwrap();

    send_messages(route).await;
    let epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let correct_root = inbox.snapshots(U256::from(epoch)).call().await.unwrap();

    advance_time(epoch_period + 15 * 60 + 10).await;
    let ts = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap().header.timestamp;
    let target = (epoch + 1) * epoch_period + 15 * 60 + 10;
    if target > ts { advance_time(target - ts).await; }

    outbox.claim(U256::from(epoch), correct_root).value(deposit).send().await.unwrap().get_receipt().await.unwrap();

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

    indexer.scan_once().await;

    let state = task_store.lock().unwrap().load();
    assert!(state.tasks.iter().any(|t| t.epoch == epoch && matches!(t.kind, TaskKind::ValidateClaim)), "ValidateClaim task should exist");

    let dispatcher = TaskDispatcher::new(c.clone(), route.clone(), task_store.clone(), claim_store.clone());
    dispatcher.process_pending().await;

    let state = task_store.lock().unwrap().load();
    assert!(state.tasks.iter().any(|t| t.epoch == epoch && matches!(t.kind, TaskKind::StartVerification)), "StartVerification task should be scheduled");

    let claim_data = claim_store.lock().unwrap().get(epoch);
    outbox.challenge(U256::from(epoch), vea_validator::contracts::Claim {
        stateRoot: correct_root,
        claimer: claim_data.claimer,
        timestampClaimed: claim_data.timestamp_claimed,
        timestampVerification: 0,
        blocknumberVerification: 0,
        honest: vea_validator::contracts::Party::None,
        challenger: Address::ZERO,
    }).value(deposit).send().await.unwrap().get_receipt().await.unwrap();

    advance_time(25 * 3600 + 60).await;

    dispatcher.process_pending().await;

    let state = task_store.lock().unwrap().load();
    assert!(!state.tasks.iter().any(|t| t.epoch == epoch && matches!(t.kind, TaskKind::StartVerification)),
        "Task should be dropped immediately when Challenged event detected");
}
