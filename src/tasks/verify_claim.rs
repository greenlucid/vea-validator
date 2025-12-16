use alloy::primitives::{FixedBytes, U256};
use crate::config::Route;
use crate::contracts::IVeaInbox;
use crate::tasks::{Task, TaskKind, TaskStore, ClaimStore, challenge};

const START_VERIFICATION_DELAY: u64 = 25 * 3600;

pub async fn execute(
    route: &Route,
    epoch: u64,
    claim_store: &ClaimStore,
    current_timestamp: u64,
    task_store: &TaskStore,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let claim_data = claim_store.get(epoch);
    let claimed_state_root = claim_data.state_root;

    let inbox = IVeaInbox::new(route.inbox_address, route.inbox_provider.clone());
    let correct_state_root = inbox.snapshots(U256::from(epoch)).call().await?;

    if correct_state_root == FixedBytes::<32>::ZERO {
        println!("[{}] No snapshot on inbox for epoch {}, cannot verify claim", route.name, epoch);
        return Ok(());
    }

    if claimed_state_root == correct_state_root {
        println!("[{}] Claim for epoch {} is VALID, scheduling startVerification in 25h", route.name, epoch);
        task_store.add_task(Task {
            epoch,
            execute_after: current_timestamp + START_VERIFICATION_DELAY,
            kind: TaskKind::StartVerification,
        });
    } else {
        println!("[{}] Claim for epoch {} is INVALID! Challenging immediately", route.name, epoch);
        println!("[{}] Claimed: {:?}, Correct: {:?}", route.name, claimed_state_root, correct_state_root);
        challenge::execute(route, epoch, claim_store).await?;
    }

    Ok(())
}
