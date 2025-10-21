mod common;

use alloy::primitives::{Address, FixedBytes, U256};
use alloy::providers::ProviderBuilder;
use serial_test::serial;
use std::str::FromStr;
use std::sync::Arc;
use vea_validator::{
    contracts::{IVeaInboxArbToEth, IVeaOutboxArbToEth, IVeaInboxArbToGnosis, IVeaOutboxArbToGnosis, IWETH},
    claim_handler::ClaimHandler,
    config::ValidatorConfig,
    startup::ensure_weth_approval,
};
use common::{TestFixture, advance_time, Provider};

#[tokio::test]
#[serial]
async fn test_challenge_uses_correct_root_from_inbox() {
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
        let test_message = alloy::primitives::Bytes::from(vec![0xAA, 0xBB, 0xCC, i]);
        inbox.sendMessage(Address::from_str("0x0000000000000000000000000000000000000001").unwrap(), test_message)
            .send().await.unwrap().get_receipt().await.unwrap();
    }

    let current_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let correct_root = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();
    assert_ne!(correct_root, FixedBytes::<32>::ZERO, "Snapshot should be saved");
    println!("Saved snapshot for epoch {}", current_epoch);

    advance_time(inbox_provider.as_ref(), epoch_period + 70).await;
    advance_time(outbox_provider.as_ref(), epoch_period + 70).await;

    let target_epoch = current_epoch;
    let dest_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let dest_timestamp = dest_block.header.timestamp;
    let target_timestamp = (target_epoch + 1) * epoch_period + 70;
    let advance_amount = target_timestamp.saturating_sub(dest_timestamp);
    println!("target_epoch={}, dest_timestamp={}, target_timestamp={}, advance_amount={}", target_epoch, dest_timestamp, target_timestamp, advance_amount);
    println!("epoch_period={}", epoch_period);
    println!("Contract expects: epoch == block.timestamp / epochPeriod - 1");
    println!("So for epoch {}, need timestamp >= {}", target_epoch, (target_epoch + 1) * epoch_period);
    println!("Current outbox timestamp: {}", dest_timestamp);
    println!("Calculated claimable epoch from timestamp: {}", dest_timestamp / epoch_period - 1);
    if advance_amount > 0 {
        advance_time(outbox_provider.as_ref(), advance_amount).await;
        let after_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
        println!("After advance, outbox timestamp: {}", after_block.header.timestamp);
        println!("After advance, claimable epoch: {}", after_block.header.timestamp / epoch_period - 1);
    }

    let wrong_root = FixedBytes::<32>::from([0xDE, 0xAD, 0xBE, 0xEF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let deposit = outbox.deposit().call().await.unwrap();
    outbox.claim(U256::from(current_epoch), wrong_root).value(deposit)
        .send().await.unwrap().get_receipt().await.unwrap();

    let claim_handler = ClaimHandler::new(route.clone(), c.wallet.clone());
    let state_root_from_handler = claim_handler.get_correct_state_root(current_epoch).await.unwrap();

    assert_eq!(state_root_from_handler, correct_root, "ClaimHandler should fetch the CORRECT root from inbox");
    assert_ne!(state_root_from_handler, wrong_root, "ClaimHandler should NOT use the malicious wrong root");

    fixture.revert_snapshots().await.unwrap();
}

#[tokio::test]
#[serial]
async fn test_weth_approval_set_on_startup_if_missing() {
    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let route = &routes[1];

    let outbox_provider = Arc::new(route.outbox_provider.clone());
    let inbox_provider = Arc::new(route.inbox_provider.clone());
    let mut fixture = TestFixture::new(outbox_provider.clone(), inbox_provider.clone());
    fixture.take_snapshots().await.unwrap();

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

    ensure_weth_approval(&c, route.outbox_provider.clone()).await.unwrap();

    let allowance_after = weth.allowance(wallet_address, route.outbox_address).call().await.unwrap();
    assert_eq!(allowance_after, U256::MAX, "Allowance should be MAX after startup");

    fixture.revert_snapshots().await.unwrap();
}

#[tokio::test]
#[serial]
async fn test_weth_approval_skipped_if_already_exists() {
    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let route = &routes[1];

    let outbox_provider = Arc::new(route.outbox_provider.clone());
    let inbox_provider = Arc::new(route.inbox_provider.clone());
    let mut fixture = TestFixture::new(outbox_provider.clone(), inbox_provider.clone());
    fixture.take_snapshots().await.unwrap();

    let weth_addr = route.weth_address.expect("Gnosis should use WETH");
    let weth = IWETH::new(weth_addr, outbox_provider.clone());
    let wallet_address = c.wallet.default_signer().address();

    let manual_approval = U256::from(1000000000u64);
    let approve_tx = weth.approve(route.outbox_address, manual_approval).from(wallet_address);
    approve_tx.send().await.unwrap().get_receipt().await.unwrap();

    ensure_weth_approval(&c, route.outbox_provider.clone()).await.unwrap();

    let final_allowance = weth.allowance(wallet_address, route.outbox_address).call().await.unwrap();
    assert_eq!(final_allowance, manual_approval, "Allowance should remain unchanged when already set");

    fixture.revert_snapshots().await.unwrap();
}
