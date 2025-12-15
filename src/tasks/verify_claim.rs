use alloy::primitives::{Address, FixedBytes, U256};
use alloy::providers::DynProvider;
use alloy::network::Ethereum;
use crate::contracts::IVeaInboxArbToEth;
use crate::tasks::{Task, TaskStore, challenge};

const START_VERIFICATION_DELAY: u64 = 25 * 3600;

pub async fn execute(
    inbox_provider: DynProvider<Ethereum>,
    inbox_address: Address,
    outbox_provider: DynProvider<Ethereum>,
    outbox_address: Address,
    weth_address: Option<Address>,
    wallet_address: Address,
    epoch: u64,
    claimed_state_root: FixedBytes<32>,
    claimer: Address,
    timestamp_claimed: u32,
    current_timestamp: u64,
    task_store: &TaskStore,
    route_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let inbox = IVeaInboxArbToEth::new(inbox_address, inbox_provider);
    let correct_state_root = inbox.snapshots(U256::from(epoch)).call().await?;

    if correct_state_root == FixedBytes::<32>::ZERO {
        println!("[{}] No snapshot on inbox for epoch {}, cannot verify claim", route_name, epoch);
        return Ok(());
    }

    if claimed_state_root == correct_state_root {
        println!("[{}] Claim for epoch {} is VALID, scheduling startVerification in 25h", route_name, epoch);
        task_store.add_task(Task::StartVerification {
            epoch,
            execute_after: current_timestamp + START_VERIFICATION_DELAY,
            state_root: claimed_state_root,
            claimer,
            timestamp_claimed,
        });
    } else {
        println!("[{}] Claim for epoch {} is INVALID! Challenging immediately", route_name, epoch);
        println!("[{}] Claimed: {:?}, Correct: {:?}", route_name, claimed_state_root, correct_state_root);
        challenge::execute(
            outbox_provider,
            outbox_address,
            weth_address,
            wallet_address,
            epoch,
            claimed_state_root,
            claimer,
            timestamp_claimed,
            route_name,
        ).await?;
    }

    Ok(())
}
