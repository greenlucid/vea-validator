use alloy::primitives::U256;
use std::sync::{Arc, Mutex};
use crate::config::Route;
use crate::contracts::IVeaInbox;
use crate::tasks::{Task, TaskKind, TaskStore, ClaimStore};

const START_VERIFICATION_DELAY: u64 = 25 * 3600;

pub async fn execute(
    route: &Route,
    epoch: u64,
    claim_store: &Arc<Mutex<ClaimStore>>,
    current_timestamp: u64,
    task_store: &Arc<Mutex<TaskStore>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let claim_data = claim_store.lock().unwrap().get(epoch);
    let claimed_state_root = claim_data.state_root;

    let inbox = IVeaInbox::new(route.inbox_address, route.inbox_provider.clone());
    let correct_state_root = inbox.snapshots(U256::from(epoch)).call().await?;

    if claimed_state_root == correct_state_root {
        println!("[{}][task::validate_claim] Epoch {} VALID", route.name, epoch);
        task_store.lock().unwrap().add_task(Task {
            epoch,
            execute_after: current_timestamp + START_VERIFICATION_DELAY,
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
