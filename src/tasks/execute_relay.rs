use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use tracing::{info, warn};
use crate::config::{Route, ValidatorConfig};
use crate::contracts::{IArbSys, INodeInterface, IOutbox};
use crate::tasks::send_tx;

const ARB_SYS: Address = Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x64]);
const NODE_INTERFACE: Address = Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xC8]);

pub async fn execute(
    config: &ValidatorConfig,
    route: &Route,
    position: U256,
    l2_sender: Address,
    dest_addr: Address,
    l2_block: u64,
    l1_block: u64,
    l2_timestamp: u64,
    amount: U256,
    data: Bytes,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outbox = IOutbox::new(config.arb_outbox, config.ethereum_provider.clone());

    let is_spent = outbox.isSpent(position).call().await?;
    if is_spent {
        info!(logger = "ExecuteRelay", route = route.name, position = %position, "Position already spent");
        return Ok(());
    }

    let (proof, root) = fetch_outbox_proof(route, position).await?;

    let root_exists = outbox.roots(root).call().await?;
    if root_exists.is_zero() {
        warn!(logger = "ExecuteRelay", route = route.name, root = %format!("{:#x}", root), "Root not yet confirmed in Outbox, rescheduling");
        return Err("RootNotConfirmed".into());
    }

    let result = send_tx(
        outbox.executeTransaction(
            proof,
            position,
            l2_sender,
            dest_addr,
            U256::from(l2_block),
            U256::from(l1_block),
            U256::from(l2_timestamp),
            amount,
            data,
        ).send().await,
        "executeTransaction",
        route.name,
    ).await;

    if let Err(e) = &result {
        warn!(logger = "ExecuteRelay", route = route.name, "Execution failed: {e}, dropping task");
        return Ok(());
    }
    result
}

async fn fetch_outbox_proof(
    route: &Route,
    position: U256,
) -> Result<(Vec<FixedBytes<32>>, FixedBytes<32>), Box<dyn std::error::Error + Send + Sync>> {
    let arb_sys = IArbSys::new(ARB_SYS, route.inbox_provider.clone());
    let state = arb_sys.sendMerkleTreeState().from(Address::ZERO).call().await?;
    let size = state.size;

    let node_interface = INodeInterface::new(NODE_INTERFACE, route.inbox_provider.clone());
    let leaf = position.to::<u64>();
    let result = node_interface.constructOutboxProof(size, leaf).call().await?;

    Ok((result.proof, result.root))
}
