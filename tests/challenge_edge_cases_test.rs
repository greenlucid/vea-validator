mod common;

use alloy::primitives::{Address, FixedBytes, U256};
use serial_test::serial;
use std::str::FromStr;
use std::sync::Arc;
use vea_validator::{
    contracts::{IVeaInboxArbToEth, IVeaOutboxArbToEth, IWETH},
    config::ValidatorConfig,
    startup::ensure_weth_approval,
};
use common::{restore_pristine, advance_time};
use alloy::providers::Provider;

#[tokio::test]
#[serial]
async fn test_challenge_uses_correct_root_from_inbox() {
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
        let test_message = alloy::primitives::Bytes::from(vec![0xAA, 0xBB, 0xCC, i]);
        inbox.sendMessage(Address::from_str("0x0000000000000000000000000000000000000001").unwrap(), test_message)
            .send().await.unwrap().get_receipt().await.unwrap();
    }

    let current_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let correct_root = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();
    assert_ne!(correct_root, FixedBytes::<32>::ZERO, "Snapshot should be saved");
    println!("Saved snapshot for epoch {}", current_epoch);

    advance_time(epoch_period + 15 * 60 + 10).await;

    let inbox_block_after = inbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let inbox_timestamp_after = inbox_block_after.header.timestamp;
    let dest_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let dest_timestamp = dest_block.header.timestamp;

    println!("Inbox timestamp after advance: {}", inbox_timestamp_after);
    println!("Outbox timestamp before sync: {}", dest_timestamp);

    if inbox_timestamp_after > dest_timestamp {
        let diff = inbox_timestamp_after - dest_timestamp;
        println!("Advancing outbox by {} seconds to match inbox", diff);
        advance_time(diff).await;
    }

    let synced_outbox_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let synced_timestamp = synced_outbox_block.header.timestamp;
    println!("After sync - Outbox timestamp: {}, Claimable epoch: {}", synced_timestamp, synced_timestamp / epoch_period - 1);

    let wrong_root = FixedBytes::<32>::from([0xDE, 0xAD, 0xBE, 0xEF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let deposit = outbox.deposit().call().await.unwrap();
    outbox.claim(U256::from(current_epoch), wrong_root).value(deposit)
        .send().await.unwrap().get_receipt().await.unwrap();

    let state_root_from_inbox = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();

    assert_eq!(state_root_from_inbox, correct_root, "Inbox should have the CORRECT root");
    assert_ne!(state_root_from_inbox, wrong_root, "Inbox root should NOT match the malicious wrong root");

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
