mod common;

use alloy::primitives::{Address, FixedBytes, U256};
use serial_test::serial;
use std::str::FromStr;
use std::sync::Arc;
use vea_validator::{
    contracts::{IVeaInboxArbToEth, IVeaOutboxArbToEth, IWETH},
    config::ValidatorConfig,
    indexer::EventIndexer,
    tasks::dispatcher::TaskDispatcher,
    startup::ensure_weth_approval,
};
use common::{restore_pristine, advance_time};
use alloy::providers::Provider;

#[tokio::test]
#[serial]
async fn test_challenge_when_claim_has_incorrect_root() {
    println!("\n==============================================");
    println!("CHALLENGE TEST: Validator Challenges Bad Claim");
    println!("==============================================\n");

    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let route = &routes[0];
    let wallet_address = c.wallet.default_signer().address();

    let inbox_provider = Arc::new(route.inbox_provider.clone());
    let outbox_provider = Arc::new(route.outbox_provider.clone());
    restore_pristine().await;

    let inbox = IVeaInboxArbToEth::new(route.inbox_address, inbox_provider.clone());
    let outbox = IVeaOutboxArbToEth::new(route.outbox_address, outbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    for i in 0..3 {
        let test_message = alloy::primitives::Bytes::from(vec![0xAA, 0xBB, 0xCC, i]);
        inbox.sendMessage(Address::from_str("0x0000000000000000000000000000000000000001").unwrap(), test_message)
            .send().await.unwrap().get_receipt().await.unwrap();
    }

    let current_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let correct_root = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();
    assert_ne!(correct_root, FixedBytes::<32>::ZERO, "Snapshot should be saved");
    println!("Epoch {} - correct root: {:?}", current_epoch, correct_root);

    advance_time(epoch_period + 15 * 60 + 10).await;

    let dest_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let dest_timestamp = dest_block.header.timestamp;
    let target_timestamp = (current_epoch + 1) * epoch_period + 15 * 60 + 10;
    let advance_amount = target_timestamp.saturating_sub(dest_timestamp);
    if advance_amount > 0 {
        advance_time(advance_amount).await;
    }

    let wrong_root = FixedBytes::<32>::from([0xDE, 0xAD, 0xBE, 0xEF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let deposit = outbox.deposit().call().await.unwrap();
    println!("Malicious actor claims epoch {} with WRONG root: {:?}", current_epoch, wrong_root);
    outbox.claim(U256::from(current_epoch), wrong_root).value(deposit)
        .send().await.unwrap().get_receipt().await.unwrap();

    let claim_hash_before = outbox.claimHashes(U256::from(current_epoch)).call().await.unwrap();
    assert_ne!(claim_hash_before, FixedBytes::<32>::ZERO, "Bad claim should exist");

    advance_time(15 * 60 + 10).await;
    println!("Advanced time 15min so indexer will process the Claimed event");

    let test_dir = tempfile::tempdir().unwrap();
    let schedule_path = test_dir.path().join("schedule.json");
    let claims_path = test_dir.path().join("claims.json");

    let indexer = EventIndexer::new(
        route.inbox_provider.clone(),
        route.inbox_address,
        route.outbox_provider.clone(),
        route.outbox_address,
        route.weth_address,
        schedule_path.clone(),
        claims_path,
        "TEST",
    );
    let dispatcher = TaskDispatcher::new(
        route.inbox_provider.clone(),
        route.inbox_address,
        route.outbox_provider.clone(),
        route.outbox_address,
        c.arb_outbox,
        route.weth_address,
        wallet_address,
        schedule_path,
        "TEST",
    );

    println!("Running indexer scan to detect the Claimed event...");
    indexer.scan_once().await;

    println!("Running dispatcher to process VerifyClaim -> Challenge...");
    dispatcher.process_pending().await;

    let challenged_sig = alloy::primitives::keccak256("Challenged(uint256,address)");
    let filter = alloy::rpc::types::Filter::new()
        .address(route.outbox_address)
        .event_signature(challenged_sig)
        .from_block(0u64);
    let logs = outbox_provider.get_logs(&filter).await.unwrap();

    if logs.is_empty() {
        panic!("VALIDATOR FAILED: Did not challenge the bad claim");
    }

    let log = &logs[0];
    let challenged_epoch = U256::from_be_bytes(log.topics()[1].0).to::<u64>();
    let challenger = Address::from_slice(&log.topics()[2].0[12..]);
    println!("Challenged event detected! epoch={}, challenger={:?}", challenged_epoch, challenger);
    assert_eq!(challenged_epoch, current_epoch, "Wrong epoch challenged");

    println!("\nCHALLENGE TEST PASSED!");
    println!("Validator correctly detected invalid claim and challenged it");
}

#[tokio::test]
#[serial]
async fn test_weth_approval_set_on_startup_if_missing() {
    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let route = &routes[1];

    let outbox_provider = Arc::new(route.outbox_provider.clone());
    restore_pristine().await;

    let weth_addr = route.weth_address.expect("Gnosis should use WETH");
    let weth = IWETH::new(weth_addr, outbox_provider.clone());
    let wallet_address = c.wallet.default_signer().address();
    let initial_allowance = weth.allowance(wallet_address, route.outbox_address).call().await.unwrap();

    if initial_allowance > U256::ZERO {
        let revoke_tx = weth.approve(route.outbox_address, U256::ZERO).from(wallet_address);
        revoke_tx.send().await.unwrap().get_receipt().await.unwrap();
    }

    let allowance_before = weth.allowance(wallet_address, route.outbox_address).call().await.unwrap();
    assert_eq!(allowance_before, U256::ZERO, "Allowance should be zero before startup");

    ensure_weth_approval(&c, route.outbox_provider.clone(), wallet_address).await.unwrap();

    let allowance_after = weth.allowance(wallet_address, route.outbox_address).call().await.unwrap();
    assert_eq!(allowance_after, U256::MAX, "Allowance should be MAX after startup");

}

#[tokio::test]
#[serial]
async fn test_weth_approval_skipped_if_already_exists() {
    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let route = &routes[1];

    let outbox_provider = Arc::new(route.outbox_provider.clone());
    restore_pristine().await;

    let weth_addr = route.weth_address.expect("Gnosis should use WETH");
    let weth = IWETH::new(weth_addr, outbox_provider.clone());
    let wallet_address = c.wallet.default_signer().address();

    let manual_approval = U256::from(1000000000u64);
    let approve_tx = weth.approve(route.outbox_address, manual_approval).from(wallet_address);
    approve_tx.send().await.unwrap().get_receipt().await.unwrap();

    ensure_weth_approval(&c, route.outbox_provider.clone(), wallet_address).await.unwrap();

    let final_allowance = weth.allowance(wallet_address, route.outbox_address).call().await.unwrap();
    assert_eq!(final_allowance, manual_approval, "Allowance should remain unchanged when already set");

}
