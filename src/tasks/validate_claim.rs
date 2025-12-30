use alloy::primitives::U256;
use std::sync::{Arc, Mutex};
use crate::config::{Route, ValidatorConfig};
use crate::contracts::IVeaInbox;
use crate::finality::is_epoch_finalized;
use crate::tasks::{Task, TaskKind, TaskStore, ClaimStore};

pub async fn execute(
    config: &ValidatorConfig,
    route: &Route,
    epoch: u64,
    claim_store: &Arc<Mutex<ClaimStore>>,
    current_timestamp: u64,
    task_store: &Arc<Mutex<TaskStore>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let inbox = IVeaInbox::new(route.inbox_address, route.inbox_provider.clone());
    let epoch_period: u64 = inbox.epochPeriod().call().await?.try_into()?;

    let finalized = is_epoch_finalized(
        epoch,
        epoch_period,
        &route.inbox_provider,
        &config.ethereum_provider,
        config.sequencer_inbox,
    ).await?;
    if !finalized {
        println!("[{}][task::validate_claim] Epoch {} not yet finalized on L1", route.name, epoch);
        return Err("EpochNotFinalized".into());
    }

    let claim_data = claim_store.lock().unwrap().get(epoch);
    let claimed_state_root = claim_data.state_root;

    let correct_state_root = inbox.snapshots(U256::from(epoch)).call().await?;

    if claimed_state_root == correct_state_root {
        println!("[{}][task::validate_claim] Epoch {} VALID", route.name, epoch);
        task_store.lock().unwrap().add_task(Task {
            epoch,
            execute_after: current_timestamp + route.settings.start_verification_delay,
            kind: TaskKind::StartVerification,
        });
    } else {
        println!("[{}][task::validate_claim] Epoch {} INVALID - scheduling challenge", route.name, epoch);
        println!("[{}][task::validate_claim] Claimed: {:?}, Correct: {:?}", route.name, claimed_state_root, correct_state_root);
        task_store.lock().unwrap().add_task(Task {
            epoch,
            execute_after: current_timestamp,
            kind: TaskKind::Challenge,
        });
    }

    Ok(())
}
