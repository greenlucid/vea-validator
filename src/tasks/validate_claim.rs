use alloy::primitives::U256;
use std::sync::{Arc, Mutex};
use tracing::{info, warn, error};
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

    let finalized = match is_epoch_finalized(
        epoch,
        epoch_period,
        &route.inbox_provider,
        &config.ethereum_provider,
        config.sequencer_inbox,
    ).await {
        Ok(f) => f,
        Err(e) => {
            error!(logger = "ValidateClaim", route = route.name, epoch, "Finality check failed: {e}");
            return Err(e);
        }
    };
    if !finalized {
        warn!(logger = "ValidateClaim", route = route.name, epoch, "Epoch not yet finalized on L1");
        return Err("EpochNotFinalized".into());
    }

    let claim_data = claim_store.lock().unwrap().get(epoch);
    let claimed_state_root = claim_data.state_root;

    let correct_state_root = inbox.snapshots(U256::from(epoch)).call().await?;

    if claimed_state_root == correct_state_root {
        info!(logger = "ValidateClaim", route = route.name, epoch, "VALID");
        task_store.lock().unwrap().add_task(Task {
            epoch,
            execute_after: current_timestamp + route.settings.start_verification_delay,
            kind: TaskKind::StartVerification,
        });
    } else {
        error!(logger = "ValidateClaim", route = route.name, epoch, claimed = ?claimed_state_root, correct = ?correct_state_root, "INVALID - scheduling challenge");
        task_store.lock().unwrap().add_task(Task {
            epoch,
            execute_after: current_timestamp,
            kind: TaskKind::Challenge,
        });
    }

    Ok(())
}
