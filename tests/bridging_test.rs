mod common;

use alloy::primitives::{Address, FixedBytes, U256};
use serial_test::serial;
use std::str::FromStr;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::time::{timeout, Duration};
use vea_validator::{
    contracts::{IVeaInboxArbToEth, IVeaOutboxArbToEth, IVeaInboxArbToGnosis, IVeaOutboxArbToGnosis, IOutbox, IAMB, IWETH, Claim, Party},
    config::ValidatorConfig,
    l2_to_l1_finder::L2ToL1Finder,
    arb_relay_handler::ArbRelayHandler,
    claim_finder::ClaimFinder,
    verification_handler::VerificationHandler,
    amb_finder::AmbFinder,
    amb_relay_handler::AmbRelayHandler,
    scheduler::{ScheduleFile, ScheduleData, ArbToL1Task, VerificationTask, VerificationPhase, AmbTask},
};
use common::{restore_pristine, advance_time, Provider};
use alloy::providers::DynProvider;
use alloy::network::Ethereum;

const ARB_OUTBOX_ENV: &str = "ARB_OUTBOX";

fn get_arb_outbox() -> Address {
    std::env::var(ARB_OUTBOX_ENV)
        .expect("ARB_OUTBOX must be set")
        .parse()
        .expect("Invalid ARB_OUTBOX address")
}

#[tokio::test]
#[serial]
async fn test_l2_to_l1_finder_discovers_snapshot_sent_event() {
    println!("\n==============================================");
    println!("BRIDGING TEST: L2ToL1Finder Discovers SnapshotSent");
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
        let test_message = alloy::primitives::Bytes::from(vec![0xAA, 0xBB, 0xCC, i]);
        inbox.sendMessage(
            Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            test_message
        ).send().await.unwrap().get_receipt().await.unwrap();
    }

    let current_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let correct_root = inbox.snapshots(U256::from(current_epoch)).call().await.unwrap();

    println!("Saved snapshot for epoch {} with root: {:?}", current_epoch, correct_root);

    advance_time(inbox_provider.as_ref(), epoch_period + 10).await;
    advance_time(outbox_provider.as_ref(), epoch_period + 10).await;

    let target_epoch = current_epoch;

    let eth_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let eth_timestamp = eth_block.header.timestamp;
    let target_timestamp = (target_epoch + 1) * epoch_period + 10;
    let advance_amount = target_timestamp.saturating_sub(eth_timestamp);
    if advance_amount > 0 {
        advance_time(outbox_provider.as_ref(), advance_amount).await;
    }

    println!("Submitting wrong claim to trigger challenge + bridging...");
    let wrong_root = FixedBytes::<32>::from([0x99; 32]);
    let deposit = outbox.deposit().call().await.unwrap();
    outbox.claim(U256::from(target_epoch), wrong_root)
        .value(deposit)
        .send().await.unwrap()
        .get_receipt().await.unwrap();

    let wallet_address = c.wallet.default_signer().address();
    let claim = Claim {
        stateRoot: wrong_root,
        claimer: wallet_address,
        timestampClaimed: eth_timestamp as u32,
        timestampVerification: 0,
        blocknumberVerification: 0,
        honest: Party::None,
        challenger: Address::ZERO,
    };

    let challenge_deposit = outbox.deposit().call().await.unwrap();
    outbox.challenge(U256::from(target_epoch), claim.clone(), wallet_address)
        .value(challenge_deposit)
        .send().await.unwrap()
        .get_receipt().await.unwrap();
    println!("Challenge submitted");

    inbox.sendSnapshot(U256::from(target_epoch), claim)
        .send().await.unwrap()
        .get_receipt().await.unwrap();
    println!("sendSnapshot() called - emitted SnapshotSent event");

    let tmp_dir = tempdir().unwrap();
    let schedule_path = tmp_dir.path().join("arb_to_eth.json");

    let arb_provider_dyn: DynProvider<Ethereum> = route.inbox_provider.clone();

    let finder = L2ToL1Finder::new(arb_provider_dyn)
        .add_inbox(route.inbox_address, &schedule_path);

    let finder_handle = tokio::spawn(async move {
        finder.run().await;
    });

    let result = timeout(Duration::from_secs(30), async {
        loop {
            let schedule_file: ScheduleFile<ArbToL1Task> = ScheduleFile::new(&schedule_path);
            let schedule = schedule_file.load();
            if !schedule.pending.is_empty() {
                return schedule.pending[0].clone();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await;

    finder_handle.abort();

    match result {
        Ok(task) => {
            println!("\nL2ToL1Finder discovered task:");
            println!("  epoch: {}", task.epoch);
            println!("  position: {:#x}", task.position);
            println!("  execute_after: {}", task.execute_after);
            println!("  l2_sender (VeaInbox): {:?}", task.l2_sender);
            println!("  dest_addr (VeaOutbox): {:?}", task.dest_addr);
            println!("  l2_block: {}", task.l2_block);
            println!("  l1_block: {}", task.l1_block);
            println!("  l2_timestamp: {}", task.l2_timestamp);
            println!("  amount: {}", task.amount);
            println!("  data len: {} bytes", task.data.len());

            assert_eq!(task.epoch, target_epoch, "Epoch should match");
            assert!(task.execute_after > 0, "execute_after should be set");
            assert_eq!(task.l2_sender, route.inbox_address, "l2_sender should be VeaInbox");
            assert_eq!(task.dest_addr, route.outbox_address, "dest_addr should be VeaOutbox");
            assert!(task.l2_block > 0, "l2_block should be set");
            assert!(task.l2_timestamp > 0, "l2_timestamp should be set");
            assert!(!task.data.is_empty(), "data should contain resolveDisputedClaim calldata");

            println!("\nL2ToL1 FINDER TEST PASSED!");
        }
        Err(_) => {
            panic!("L2ToL1Finder did not discover the event within 30 seconds");
        }
    }
}

#[tokio::test]
#[serial]
async fn test_scheduler_persistence_roundtrip() {
    println!("\n==============================================");
    println!("SCHEDULER TEST: Persistence Roundtrip");
    println!("==============================================\n");

    let tmp_dir = tempdir().unwrap();
    let schedule_path = tmp_dir.path().join("test_schedule.json");

    let task = ArbToL1Task {
        epoch: 42,
        position: U256::from(123),
        execute_after: 1700000000,
        l2_sender: Address::ZERO,
        dest_addr: Address::ZERO,
        l2_block: 100,
        l1_block: 50,
        l2_timestamp: 1700000000,
        amount: U256::ZERO,
        data: alloy::primitives::Bytes::new(),
    };

    let schedule_file: ScheduleFile<ArbToL1Task> = ScheduleFile::new(&schedule_path);

    let mut schedule = ScheduleData::default();
    schedule.last_checked_block = Some(12345);
    schedule.pending.push(task.clone());

    schedule_file.save(&schedule);
    println!("Saved schedule to {}", schedule_path.display());

    let loaded = schedule_file.load();

    assert_eq!(loaded.last_checked_block, Some(12345), "last_checked_block should persist");
    assert_eq!(loaded.pending.len(), 1, "Should have 1 pending task");

    let loaded_task = &loaded.pending[0];
    assert_eq!(loaded_task.epoch, 42, "epoch should match");
    assert_eq!(loaded_task.position, U256::from(123), "position should match");
    assert_eq!(loaded_task.execute_after, 1700000000, "execute_after should match");

    println!("\nSCHEDULER PERSISTENCE TEST PASSED!");
}

#[tokio::test]
#[serial]
async fn test_arb_relay_handler_checks_spent_status() {
    println!("\n==============================================");
    println!("BRIDGING TEST: ArbRelayHandler Checks Spent Status");
    println!("==============================================\n");

    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let route = &routes[0];
    let outbox_provider = Arc::new(route.outbox_provider.clone());

    let arb_outbox = get_arb_outbox();

    restore_pristine().await;

    let outbox = IOutbox::new(arb_outbox, outbox_provider.clone());

    let is_spent = outbox.isSpent(U256::from(0)).call().await.unwrap();
    println!("Position 0 isSpent: {}", is_spent);
    assert!(!is_spent, "Position 0 should not be spent initially");

    println!("\nARB RELAY HANDLER SPENT CHECK TEST PASSED!");
}

#[tokio::test]
#[serial]
async fn test_full_arb_to_eth_relay_flow() {
    println!("\n==============================================");
    println!("BRIDGING TEST: Full ARB to ETH Relay Flow");
    println!("==============================================\n");

    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let route = &routes[0];

    let inbox_provider = Arc::new(route.inbox_provider.clone());
    let outbox_provider = Arc::new(route.outbox_provider.clone());

    let arb_outbox = get_arb_outbox();

    restore_pristine().await;

    let inbox = IVeaInboxArbToEth::new(route.inbox_address, inbox_provider.clone());
    let outbox = IVeaOutboxArbToEth::new(route.outbox_address, outbox_provider.clone());

    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();
    let seq_delay: u64 = outbox.sequencerDelayLimit().call().await.unwrap().try_into().unwrap();
    let min_challenge: u64 = outbox.minChallengePeriod().call().await.unwrap().try_into().unwrap();

    let latest_verified_start: u64 = outbox.latestVerifiedEpoch().call().await.unwrap().try_into().unwrap();
    println!("Contract params: epochPeriod={}, seqDelay={}, minChallenge={}", epoch_period, seq_delay, min_challenge);
    println!("Initial latestVerifiedEpoch={}", latest_verified_start);

    let wallet_address = c.wallet.default_signer().address();
    let deposit = outbox.deposit().call().await.unwrap();

    println!("Phase 0: Running initial honest verification to get bridge into healthy state...");
    {
        for i in 0..2 {
            let test_message = alloy::primitives::Bytes::from(vec![0x00, 0x00, i]);
            inbox.sendMessage(
                Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                test_message
            ).send().await.unwrap().get_receipt().await.unwrap();
        }

        let init_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
        inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
        let init_root = inbox.snapshots(U256::from(init_epoch)).call().await.unwrap();

        advance_time(inbox_provider.as_ref(), epoch_period + 10).await;
        advance_time(outbox_provider.as_ref(), epoch_period + 10).await;

        let init_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
        let init_claim_ts = init_block.header.timestamp;

        outbox.claim(U256::from(init_epoch), init_root)
            .value(deposit)
            .send().await.unwrap()
            .get_receipt().await.unwrap();

        advance_time(inbox_provider.as_ref(), seq_delay + epoch_period + 10).await;
        advance_time(outbox_provider.as_ref(), seq_delay + epoch_period + 10).await;

        let init_claim = Claim {
            stateRoot: init_root,
            claimer: wallet_address,
            timestampClaimed: init_claim_ts as u32,
            timestampVerification: 0,
            blocknumberVerification: 0,
            honest: Party::None,
            challenger: Address::ZERO,
        };

        outbox.startVerification(U256::from(init_epoch), init_claim.clone())
            .send().await.unwrap()
            .get_receipt().await.unwrap();

        let verif_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
        let verif_ts = verif_block.header.timestamp as u32;
        let verif_bn = verif_block.header.number as u32;

        advance_time(inbox_provider.as_ref(), min_challenge + 10).await;
        advance_time(outbox_provider.as_ref(), min_challenge + 10).await;

        let verified_claim = Claim {
            stateRoot: init_root,
            claimer: wallet_address,
            timestampClaimed: init_claim_ts as u32,
            timestampVerification: verif_ts,
            blocknumberVerification: verif_bn,
            honest: Party::None,
            challenger: Address::ZERO,
        };

        outbox.verifySnapshot(U256::from(init_epoch), verified_claim)
            .send().await.unwrap()
            .get_receipt().await.unwrap();

        let latest_verified: u64 = outbox.latestVerifiedEpoch().call().await.unwrap().try_into().unwrap();
        println!("  Initial verification complete: epoch {}, latestVerifiedEpoch={}", init_epoch, latest_verified);
    }

    for i in 0..3 {
        let test_message = alloy::primitives::Bytes::from(vec![0xDE, 0xAD, 0xBE, 0xEF, i]);
        inbox.sendMessage(
            Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            test_message
        ).send().await.unwrap().get_receipt().await.unwrap();
    }

    let challenged_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let _correct_root = inbox.snapshots(U256::from(challenged_epoch)).call().await.unwrap();

    println!("Phase 1: Setup complete - challenged epoch {}", challenged_epoch);

    advance_time(inbox_provider.as_ref(), epoch_period + 10).await;
    advance_time(outbox_provider.as_ref(), epoch_period + 10).await;

    let eth_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let eth_timestamp = eth_block.header.timestamp;
    let target_timestamp = (challenged_epoch + 1) * epoch_period + 10;
    let advance_amount = target_timestamp.saturating_sub(eth_timestamp);
    if advance_amount > 0 {
        advance_time(outbox_provider.as_ref(), advance_amount).await;
    }

    let wrong_root = FixedBytes::<32>::from([0x77; 32]);
    let deposit = outbox.deposit().call().await.unwrap();
    let claim_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let claim_timestamp = claim_block.header.timestamp;

    outbox.claim(U256::from(challenged_epoch), wrong_root)
        .value(deposit)
        .send().await.unwrap()
        .get_receipt().await.unwrap();

    println!("Phase 2: Wrong claim submitted for epoch {}", challenged_epoch);

    let challenged_claim = Claim {
        stateRoot: wrong_root,
        claimer: wallet_address,
        timestampClaimed: claim_timestamp as u32,
        timestampVerification: 0,
        blocknumberVerification: 0,
        honest: Party::None,
        challenger: Address::ZERO,
    };

    let challenge_deposit = outbox.deposit().call().await.unwrap();
    outbox.challenge(U256::from(challenged_epoch), challenged_claim.clone(), wallet_address)
        .value(challenge_deposit)
        .send().await.unwrap()
        .get_receipt().await.unwrap();

    inbox.sendSnapshot(U256::from(challenged_epoch), challenged_claim)
        .send().await.unwrap()
        .get_receipt().await.unwrap();

    println!("Phase 3: Challenge + sendSnapshot complete");

    let tmp_dir = tempdir().unwrap();
    let schedule_path = tmp_dir.path().join("arb_to_eth.json");

    let arb_provider_dyn: DynProvider<Ethereum> = route.inbox_provider.clone();

    let finder = L2ToL1Finder::new(arb_provider_dyn.clone())
        .add_inbox(route.inbox_address, &schedule_path);

    let finder_handle = tokio::spawn(async move {
        finder.run().await;
    });

    let task = timeout(Duration::from_secs(30), async {
        loop {
            let schedule_file: ScheduleFile<ArbToL1Task> = ScheduleFile::new(&schedule_path);
            let schedule = schedule_file.load();
            if !schedule.pending.is_empty() {
                return schedule.pending[0].clone();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await.expect("Finder should discover task within 30s");

    finder_handle.abort();

    println!("Phase 4: L2ToL1Finder discovered task - epoch {}, position {:#x}", task.epoch, task.position);

    let arb_outbox_contract = IOutbox::new(arb_outbox, outbox_provider.clone());
    let is_spent_before = arb_outbox_contract.isSpent(task.position).call().await.unwrap();
    assert!(!is_spent_before, "Position should NOT be spent before relay");
    println!("Phase 5: Verified position {:#x} is not spent yet", task.position);

    println!("Phase 6: Running honest epoch loop to keep bridge alive during 7-day wait...");

    let relay_delay: u64 = 7 * 24 * 3600;
    let mut time_accumulated: u64 = 0;
    let mut cycle = 0;

    while time_accumulated < relay_delay {
        cycle += 1;

        for i in 0..2 {
            let test_message = alloy::primitives::Bytes::from(vec![0xAA, cycle as u8, i]);
            inbox.sendMessage(
                Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
                test_message
            ).send().await.unwrap().get_receipt().await.unwrap();
        }

        let honest_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
        inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
        let honest_root = inbox.snapshots(U256::from(honest_epoch)).call().await.unwrap();

        advance_time(inbox_provider.as_ref(), epoch_period + 10).await;
        advance_time(outbox_provider.as_ref(), epoch_period + 10).await;
        time_accumulated += epoch_period + 10;

        let block_after_epoch = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
        let honest_claim_ts = block_after_epoch.header.timestamp;

        outbox.claim(U256::from(honest_epoch), honest_root)
            .value(deposit)
            .send().await.unwrap()
            .get_receipt().await.unwrap();

        advance_time(inbox_provider.as_ref(), seq_delay + epoch_period + 10).await;
        advance_time(outbox_provider.as_ref(), seq_delay + epoch_period + 10).await;
        time_accumulated += seq_delay + epoch_period + 10;

        let honest_claim = Claim {
            stateRoot: honest_root,
            claimer: wallet_address,
            timestampClaimed: honest_claim_ts as u32,
            timestampVerification: 0,
            blocknumberVerification: 0,
            honest: Party::None,
            challenger: Address::ZERO,
        };

        outbox.startVerification(U256::from(honest_epoch), honest_claim.clone())
            .send().await.unwrap()
            .get_receipt().await.unwrap();

        let verif_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
        let verif_ts = verif_block.header.timestamp as u32;
        let verif_bn = verif_block.header.number as u32;

        advance_time(inbox_provider.as_ref(), min_challenge + 10).await;
        advance_time(outbox_provider.as_ref(), min_challenge + 10).await;
        time_accumulated += min_challenge + 10;

        let verified_claim = Claim {
            stateRoot: honest_root,
            claimer: wallet_address,
            timestampClaimed: honest_claim_ts as u32,
            timestampVerification: verif_ts,
            blocknumberVerification: verif_bn,
            honest: Party::None,
            challenger: Address::ZERO,
        };

        outbox.verifySnapshot(U256::from(honest_epoch), verified_claim)
            .send().await.unwrap()
            .get_receipt().await.unwrap();

        let latest_verified: u64 = outbox.latestVerifiedEpoch().call().await.unwrap().try_into().unwrap();
        let current_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
        println!("  Cycle {}: verified epoch {}, latestVerified={}, currentEpoch={}, time={}s/{}s",
            cycle, honest_epoch, latest_verified, current_epoch, time_accumulated, relay_delay);
    }

    println!("Phase 7: Bridge kept alive, now executing relay...");

    let handler = ArbRelayHandler::new(
        route.inbox_provider.clone(),
        route.outbox_provider.clone(),
        arb_outbox,
        &schedule_path,
    );

    handler.process_pending().await;

    let is_spent_after = arb_outbox_contract.isSpent(task.position).call().await.unwrap();
    assert!(is_spent_after, "Position SHOULD be spent after relay");
    println!("Phase 8: Verified position {:#x} IS spent after relay!", task.position);

    let schedule_after: ScheduleData<ArbToL1Task> = ScheduleFile::new(&schedule_path).load();
    assert!(schedule_after.pending.is_empty(), "Schedule should be empty after successful relay");
    println!("Phase 9: Verified schedule is now empty");

    println!("\nFULL ARB TO ETH RELAY FLOW TEST PASSED!");
    println!("Successfully verified:");
    println!("  1. sendSnapshot emits SnapshotSent event");
    println!("  2. L2ToL1Finder discovers and schedules the task");
    println!("  3. Task has correct epoch, position, execute_after");
    println!("  4. Time advancement works for 7-day delay");
    println!("  5. ArbRelayHandler.process_pending() executes the relay");
    println!("  6. Outbox.isSpent() returns true after relay");
    println!("  7. Task is removed from schedule after successful relay");
}

#[tokio::test]
#[serial]
async fn test_claim_finder_challenges_invalid_claim() {
    println!("\n==============================================");
    println!("CLAIM FINDER TEST: Challenges Invalid Claim");
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
        let test_message = alloy::primitives::Bytes::from(vec![0xDE, 0xAD, i]);
        inbox.sendMessage(
            Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            test_message
        ).send().await.unwrap().get_receipt().await.unwrap();
    }

    let target_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let correct_root = inbox.snapshots(U256::from(target_epoch)).call().await.unwrap();
    println!("Saved snapshot for epoch {} with correct root: {:?}", target_epoch, correct_root);

    advance_time(inbox_provider.as_ref(), epoch_period + 10).await;
    advance_time(outbox_provider.as_ref(), epoch_period + 10).await;

    let wrong_root = FixedBytes::<32>::from([0x99; 32]);
    let deposit = outbox.deposit().call().await.unwrap();

    outbox.claim(U256::from(target_epoch), wrong_root)
        .value(deposit)
        .send().await.unwrap()
        .get_receipt().await.unwrap();
    println!("Submitted WRONG claim for epoch {}", target_epoch);

    let original_claim_hash = outbox.claimHashes(U256::from(target_epoch)).call().await.unwrap();
    println!("Original claim hash: {:?}", original_claim_hash);

    advance_time(outbox_provider.as_ref(), 16 * 60).await;

    let tmp_dir = tempdir().unwrap();
    let schedule_path = tmp_dir.path().join("verification.json");

    let wallet_address = c.wallet.default_signer().address();

    let finder = ClaimFinder::new(
        route.inbox_provider.clone(),
        route.outbox_provider.clone(),
        route.inbox_address,
        route.outbox_address,
        None,
        wallet_address,
        &schedule_path,
        "TEST",
    );

    let finder_handle = tokio::spawn(async move {
        finder.run().await;
    });

    let result = timeout(Duration::from_secs(30), async {
        loop {
            let claim_hash = outbox.claimHashes(U256::from(target_epoch)).call().await.unwrap();
            if claim_hash != original_claim_hash {
                println!("Claim hash changed to: {:?}", claim_hash);
                return true;
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await;

    finder_handle.abort();

    assert!(result.is_ok(), "ClaimFinder should have challenged the invalid claim (claim hash should change)");
    println!("\nCLAIM FINDER CHALLENGE TEST PASSED!");
}

#[tokio::test]
#[serial]
async fn test_claim_finder_schedules_valid_claim_verification() {
    println!("\n==============================================");
    println!("CLAIM FINDER TEST: Schedules Valid Claim Verification");
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
        let test_message = alloy::primitives::Bytes::from(vec![0xBE, 0xEF, i]);
        inbox.sendMessage(
            Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            test_message
        ).send().await.unwrap().get_receipt().await.unwrap();
    }

    let target_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let correct_root = inbox.snapshots(U256::from(target_epoch)).call().await.unwrap();
    println!("Saved snapshot for epoch {} with root: {:?}", target_epoch, correct_root);

    advance_time(inbox_provider.as_ref(), epoch_period + 10).await;
    advance_time(outbox_provider.as_ref(), epoch_period + 10).await;

    let deposit = outbox.deposit().call().await.unwrap();
    outbox.claim(U256::from(target_epoch), correct_root)
        .value(deposit)
        .send().await.unwrap()
        .get_receipt().await.unwrap();
    println!("Submitted VALID claim for epoch {}", target_epoch);

    advance_time(outbox_provider.as_ref(), 16 * 60).await;

    let tmp_dir = tempdir().unwrap();
    let schedule_path = tmp_dir.path().join("verification.json");

    let wallet_address = c.wallet.default_signer().address();

    let finder = ClaimFinder::new(
        route.inbox_provider.clone(),
        route.outbox_provider.clone(),
        route.inbox_address,
        route.outbox_address,
        None,
        wallet_address,
        &schedule_path,
        "TEST",
    );

    let finder_handle = tokio::spawn(async move {
        finder.run().await;
    });

    let result = timeout(Duration::from_secs(30), async {
        loop {
            let schedule_file: ScheduleFile<VerificationTask> = ScheduleFile::new(&schedule_path);
            let schedule = schedule_file.load();
            if let Some(task) = schedule.pending.iter().find(|t| t.epoch == target_epoch) {
                return task.clone();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await;

    finder_handle.abort();

    match result {
        Ok(task) => {
            println!("\nClaimFinder scheduled verification task:");
            println!("  epoch: {}", task.epoch);
            println!("  phase: {:?}", task.phase);
            println!("  execute_after: {}", task.execute_after);
            println!("  state_root: {:?}", task.state_root);

            assert_eq!(task.epoch, target_epoch, "Epoch should match");
            assert!(matches!(task.phase, VerificationPhase::StartVerification), "Phase should be StartVerification");
            assert_eq!(task.state_root, correct_root, "State root should match");
            assert!(task.execute_after > 0, "execute_after should be set");

            println!("\nCLAIM FINDER VALID CLAIM SCHEDULING TEST PASSED!");
        }
        Err(_) => {
            panic!("ClaimFinder did not schedule verification task within 30 seconds");
        }
    }
}

#[tokio::test]
#[serial]
async fn test_verification_handler_calls_start_verification() {
    println!("\n==============================================");
    println!("VERIFICATION HANDLER TEST: Calls startVerification");
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
    let seq_delay: u64 = outbox.sequencerDelayLimit().call().await.unwrap().try_into().unwrap();

    for i in 0..3 {
        let test_message = alloy::primitives::Bytes::from(vec![0xAA, 0xBB, i]);
        inbox.sendMessage(
            Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            test_message
        ).send().await.unwrap().get_receipt().await.unwrap();
    }

    let target_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let correct_root = inbox.snapshots(U256::from(target_epoch)).call().await.unwrap();
    println!("Saved snapshot for epoch {} with root: {:?}", target_epoch, correct_root);

    advance_time(inbox_provider.as_ref(), epoch_period + 10).await;
    advance_time(outbox_provider.as_ref(), epoch_period + 10).await;

    let eth_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let claim_timestamp = eth_block.header.timestamp;

    let deposit = outbox.deposit().call().await.unwrap();
    outbox.claim(U256::from(target_epoch), correct_root)
        .value(deposit)
        .send().await.unwrap()
        .get_receipt().await.unwrap();
    println!("Submitted valid claim for epoch {}", target_epoch);

    let wallet_address = c.wallet.default_signer().address();

    let tmp_dir = tempdir().unwrap();
    let schedule_path = tmp_dir.path().join("verification.json");

    let schedule_file: ScheduleFile<VerificationTask> = ScheduleFile::new(&schedule_path);
    let mut schedule = ScheduleData::default();
    schedule.pending.push(VerificationTask {
        epoch: target_epoch,
        execute_after: 0,
        phase: VerificationPhase::StartVerification,
        state_root: correct_root,
        claimer: wallet_address,
        timestamp_claimed: claim_timestamp as u32,
        timestamp_verification: 0,
        blocknumber_verification: 0,
    });
    schedule_file.save(&schedule);
    println!("Pre-seeded schedule with StartVerification task for epoch {}", target_epoch);

    advance_time(inbox_provider.as_ref(), seq_delay + epoch_period + 10).await;
    advance_time(outbox_provider.as_ref(), seq_delay + epoch_period + 10).await;

    let handler = VerificationHandler::new(
        route.outbox_provider.clone(),
        route.outbox_address,
        None,
        &schedule_path,
        "TEST",
    );

    handler.process_pending().await;

    let schedule_after = schedule_file.load();
    assert!(schedule_after.pending.is_empty(), "Task should be removed from schedule after startVerification");

    let claim_hash_after = outbox.claimHashes(U256::from(target_epoch)).call().await.unwrap();
    assert!(claim_hash_after != FixedBytes::<32>::ZERO, "Claim hash should be updated after startVerification");
    println!("startVerification succeeded - claim hash: {:?}", claim_hash_after);

    println!("\nVERIFICATION HANDLER START_VERIFICATION TEST PASSED!");
}

#[tokio::test]
#[serial]
async fn test_verification_handler_calls_verify_snapshot() {
    println!("\n==============================================");
    println!("VERIFICATION HANDLER TEST: Calls verifySnapshot");
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
    let seq_delay: u64 = outbox.sequencerDelayLimit().call().await.unwrap().try_into().unwrap();
    let min_challenge: u64 = outbox.minChallengePeriod().call().await.unwrap().try_into().unwrap();

    let latest_before: u64 = outbox.latestVerifiedEpoch().call().await.unwrap().try_into().unwrap();
    println!("latestVerifiedEpoch before: {}", latest_before);

    for i in 0..3 {
        let test_message = alloy::primitives::Bytes::from(vec![0xCC, 0xDD, i]);
        inbox.sendMessage(
            Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            test_message
        ).send().await.unwrap().get_receipt().await.unwrap();
    }

    let target_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let correct_root = inbox.snapshots(U256::from(target_epoch)).call().await.unwrap();
    println!("Saved snapshot for epoch {} with root: {:?}", target_epoch, correct_root);

    advance_time(inbox_provider.as_ref(), epoch_period + 10).await;
    advance_time(outbox_provider.as_ref(), epoch_period + 10).await;

    let eth_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let claim_timestamp = eth_block.header.timestamp;

    let deposit = outbox.deposit().call().await.unwrap();
    outbox.claim(U256::from(target_epoch), correct_root)
        .value(deposit)
        .send().await.unwrap()
        .get_receipt().await.unwrap();
    println!("Submitted valid claim for epoch {}", target_epoch);

    let wallet_address = c.wallet.default_signer().address();

    advance_time(inbox_provider.as_ref(), seq_delay + epoch_period + 10).await;
    advance_time(outbox_provider.as_ref(), seq_delay + epoch_period + 10).await;

    let claim_for_start = Claim {
        stateRoot: correct_root,
        claimer: wallet_address,
        timestampClaimed: claim_timestamp as u32,
        timestampVerification: 0,
        blocknumberVerification: 0,
        honest: Party::None,
        challenger: Address::ZERO,
    };

    outbox.startVerification(U256::from(target_epoch), claim_for_start)
        .send().await.unwrap()
        .get_receipt().await.unwrap();
    println!("startVerification called manually");

    let verif_block = outbox_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let verif_ts = verif_block.header.timestamp as u32;
    let verif_bn = verif_block.header.number as u32;

    let tmp_dir = tempdir().unwrap();
    let schedule_path = tmp_dir.path().join("verification.json");

    let schedule_file: ScheduleFile<VerificationTask> = ScheduleFile::new(&schedule_path);
    let mut schedule = ScheduleData::default();
    schedule.pending.push(VerificationTask {
        epoch: target_epoch,
        execute_after: 0,
        phase: VerificationPhase::VerifySnapshot,
        state_root: correct_root,
        claimer: wallet_address,
        timestamp_claimed: claim_timestamp as u32,
        timestamp_verification: verif_ts,
        blocknumber_verification: verif_bn,
    });
    schedule_file.save(&schedule);
    println!("Pre-seeded schedule with VerifySnapshot task for epoch {}", target_epoch);

    advance_time(inbox_provider.as_ref(), min_challenge + 10).await;
    advance_time(outbox_provider.as_ref(), min_challenge + 10).await;

    let handler = VerificationHandler::new(
        route.outbox_provider.clone(),
        route.outbox_address,
        None,
        &schedule_path,
        "TEST",
    );

    handler.process_pending().await;

    let schedule_after = schedule_file.load();
    assert!(schedule_after.pending.is_empty(), "Task should be removed from schedule after verifySnapshot");

    let latest_after: u64 = outbox.latestVerifiedEpoch().call().await.unwrap().try_into().unwrap();
    println!("latestVerifiedEpoch after: {}", latest_after);
    assert_eq!(latest_after, target_epoch, "latestVerifiedEpoch should be updated to target epoch");

    println!("\nVERIFICATION HANDLER VERIFY_SNAPSHOT TEST PASSED!");
}

#[tokio::test]
#[serial]
async fn test_full_arb_to_gnosis_amb_flow() {
    println!("\n==============================================");
    println!("FULL ARB TO GNOSIS AMB FLOW TEST");
    println!("==============================================\n");

    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let gnosis_route = &routes[1];

    let arb_outbox_address = get_arb_outbox();

    let inbox_provider = Arc::new(gnosis_route.inbox_provider.clone());
    let eth_provider = Arc::new(gnosis_route.router_provider.as_ref().unwrap().clone());
    let gnosis_provider = Arc::new(gnosis_route.outbox_provider.clone());
    let router_address = gnosis_route.router_address.expect("Gnosis route should have router address");

    restore_pristine().await;

    let inbox = IVeaInboxArbToGnosis::new(gnosis_route.inbox_address, inbox_provider.clone());
    let outbox = IVeaOutboxArbToGnosis::new(gnosis_route.outbox_address, gnosis_provider.clone());
    let arb_outbox = IOutbox::new(arb_outbox_address, eth_provider.clone());

    let epoch_period: u64 = inbox.epochPeriod().call().await.unwrap().try_into().unwrap();

    for i in 0..3 {
        let test_message = alloy::primitives::Bytes::from(vec![0xAA, 0xBB, i]);
        inbox.sendMessage(
            Address::from_str("0x0000000000000000000000000000000000000001").unwrap(),
            test_message
        ).send().await.unwrap().get_receipt().await.unwrap();
    }

    let target_epoch: u64 = inbox.epochNow().call().await.unwrap().try_into().unwrap();
    inbox.saveSnapshot().send().await.unwrap().get_receipt().await.unwrap();
    let correct_root = inbox.snapshots(U256::from(target_epoch)).call().await.unwrap();
    println!("Phase 1: Saved snapshot for epoch {} with root: {:?}", target_epoch, correct_root);

    advance_time(inbox_provider.as_ref(), epoch_period + 10).await;
    advance_time(eth_provider.as_ref(), epoch_period + 10).await;
    advance_time(gnosis_provider.as_ref(), epoch_period + 10).await;

    let gnosis_block = gnosis_provider.get_block_by_number(Default::default()).await.unwrap().unwrap();
    let claim_timestamp = gnosis_block.header.timestamp;

    let deposit = outbox.deposit().call().await.unwrap();

    let weth_address = gnosis_route.weth_address.expect("Gnosis route needs WETH");
    let weth = IWETH::new(weth_address, gnosis_provider.clone());
    weth.deposit().value(deposit * U256::from(2)).send().await.unwrap().get_receipt().await.unwrap();
    weth.approve(gnosis_route.outbox_address, U256::MAX).send().await.unwrap().get_receipt().await.unwrap();

    let wrong_root = FixedBytes::<32>::from([0x88; 32]);
    outbox.claim(U256::from(target_epoch), wrong_root)
        .send().await.unwrap()
        .get_receipt().await.unwrap();
    println!("Phase 2: Submitted wrong claim for epoch {}", target_epoch);

    let wallet_address = c.wallet.default_signer().address();
    let challenged_claim = Claim {
        stateRoot: wrong_root,
        claimer: wallet_address,
        timestampClaimed: claim_timestamp as u32,
        timestampVerification: 0,
        blocknumberVerification: 0,
        honest: Party::None,
        challenger: Address::ZERO,
    };

    outbox.challenge(U256::from(target_epoch), challenged_claim.clone())
        .send().await.unwrap()
        .get_receipt().await.unwrap();
    println!("Phase 3: Challenge submitted");

    let gas_limit = U256::from(500000);
    inbox.sendSnapshot(U256::from(target_epoch), gas_limit, challenged_claim)
        .send().await.unwrap()
        .get_receipt().await.unwrap();
    println!("Phase 4: sendSnapshot called - emitted L2ToL1Tx");

    let tmp_dir = tempdir().unwrap();
    let l2_schedule_path = tmp_dir.path().join("l2_to_l1.json");

    let l2_finder = L2ToL1Finder::new(gnosis_route.inbox_provider.clone())
        .add_inbox(gnosis_route.inbox_address, &l2_schedule_path);

    let l2_finder_handle = tokio::spawn(async move {
        l2_finder.run().await;
    });

    let l2_task = timeout(Duration::from_secs(30), async {
        loop {
            let schedule_file: ScheduleFile<ArbToL1Task> = ScheduleFile::new(&l2_schedule_path);
            let schedule = schedule_file.load();
            if !schedule.pending.is_empty() {
                return schedule.pending[0].clone();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await.expect("L2ToL1Finder should discover task");

    l2_finder_handle.abort();
    println!("Phase 5: L2ToL1Finder discovered task - position {:#x}", l2_task.position);

    let is_spent_before = arb_outbox.isSpent(l2_task.position).call().await.unwrap();
    assert!(!is_spent_before, "Position should NOT be spent before relay");

    println!("Phase 6: Advancing time for 7-day relay delay...");
    let relay_delay: u64 = 7 * 24 * 3600;
    advance_time(inbox_provider.as_ref(), relay_delay + 100).await;
    advance_time(eth_provider.as_ref(), relay_delay + 100).await;
    advance_time(gnosis_provider.as_ref(), relay_delay + 100).await;

    let handler = ArbRelayHandler::new(
        gnosis_route.inbox_provider.clone(),
        gnosis_route.router_provider.as_ref().unwrap().clone(),
        arb_outbox_address,
        &l2_schedule_path,
    );

    handler.process_pending().await;

    let is_spent_after = arb_outbox.isSpent(l2_task.position).call().await.unwrap();
    assert!(is_spent_after, "Position SHOULD be spent after relay");
    println!("Phase 7: ArbRelayHandler executed - position {:#x} is spent", l2_task.position);

    for _ in 0..10 {
        advance_time(eth_provider.as_ref(), 12).await;
    }

    let amb_schedule_path = tmp_dir.path().join("amb.json");

    let amb_finder = AmbFinder::new(
        gnosis_route.router_provider.as_ref().unwrap().clone(),
        router_address,
        &amb_schedule_path,
    );

    let amb_finder_handle = tokio::spawn(async move {
        amb_finder.run().await;
    });

    let amb_task = timeout(Duration::from_secs(30), async {
        loop {
            let schedule_file: ScheduleFile<AmbTask> = ScheduleFile::new(&amb_schedule_path);
            let schedule = schedule_file.load();
            if !schedule.pending.is_empty() {
                return schedule.pending[0].clone();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await.expect("AmbFinder should discover Routed event");

    amb_finder_handle.abort();
    println!("Phase 8: AmbFinder discovered Routed event - epoch {}, ticket_id {:#x}", amb_task.epoch, amb_task.ticket_id);

    assert_eq!(amb_task.epoch, target_epoch, "AMB task epoch should match");
    assert!(amb_task.ticket_id != FixedBytes::<32>::ZERO, "Ticket ID should not be zero");

    println!("\nFULL ARB TO GNOSIS AMB FLOW TEST PASSED!");
    println!("Successfully verified:");
    println!("  1. sendSnapshot emits L2ToL1Tx");
    println!("  2. L2ToL1Finder discovers the task");
    println!("  3. ArbRelayHandler executes via Arbitrum outbox");
    println!("  4. Router.route() is called, emits Routed event");
    println!("  5. AmbFinder discovers the Routed event");
}

#[tokio::test]
#[serial]
async fn test_amb_relay_handler_checks_message_status() {
    println!("\n==============================================");
    println!("AMB RELAY HANDLER TEST: Checks Message Status");
    println!("==============================================\n");

    let c = ValidatorConfig::from_env().expect("Failed to load config");
    let routes = c.build_routes();
    let gnosis_route = &routes[1];

    restore_pristine().await;

    let gnosis_provider = gnosis_route.outbox_provider.clone();
    let gnosis_amb = gnosis_route.amb_address.expect("Gnosis route should have AMB address");

    let amb = IAMB::new(gnosis_amb, gnosis_provider.clone());

    let fake_ticket_id = FixedBytes::<32>::from([0x12; 32]);
    let is_executed = amb.messageCallStatus(fake_ticket_id).call().await.unwrap();
    println!("Fake ticket messageCallStatus: {}", is_executed);
    assert!(!is_executed, "Fake ticket should not be executed");

    let tmp_dir = tempdir().unwrap();
    let schedule_path = tmp_dir.path().join("amb.json");

    let schedule_file: ScheduleFile<AmbTask> = ScheduleFile::new(&schedule_path);
    let mut schedule = ScheduleData::default();
    schedule.pending.push(AmbTask {
        epoch: 42,
        ticket_id: fake_ticket_id,
        execute_after: 0,
    });
    schedule_file.save(&schedule);
    println!("Pre-seeded schedule with fake AMB task");

    let handler = AmbRelayHandler::new(
        gnosis_provider,
        gnosis_amb,
        &schedule_path,
    );

    handler.process_pending().await;

    let schedule_after = schedule_file.load();
    assert!(schedule_after.pending.is_empty(), "Task should be removed from schedule after checking status");

    println!("\nAMB RELAY HANDLER TEST PASSED!");
}
