use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use alloy::providers::DynProvider;
use alloy::network::Ethereum;
use crate::contracts::{IArbSys, INodeInterface, IOutbox};

const ARB_SYS: Address = Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x64]);
const NODE_INTERFACE: Address = Address::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xC8]);

pub async fn execute(
    arb_provider: DynProvider<Ethereum>,
    eth_provider: DynProvider<Ethereum>,
    arb_outbox_address: Address,
    epoch: u64,
    position: U256,
    l2_sender: Address,
    dest_addr: Address,
    l2_block: u64,
    l1_block: u64,
    l2_timestamp: u64,
    amount: U256,
    data: Bytes,
    route_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let outbox = IOutbox::new(arb_outbox_address, eth_provider);

    let is_spent = outbox.isSpent(position).call().await?;
    if is_spent {
        println!("[{}] Epoch {} already relayed (position {:#x})", route_name, epoch, position);
        return Ok(());
    }

    println!(
        "[{}] Executing relay for epoch {} (position {:#x})\n  l2_sender: {:?}\n  dest_addr: {:?}\n  data len: {}",
        route_name, epoch, position, l2_sender, dest_addr, data.len()
    );

    let proof = fetch_outbox_proof(arb_provider, position).await?;

    match outbox
        .executeTransaction(
            proof,
            position,
            l2_sender,
            dest_addr,
            U256::from(l2_block),
            U256::from(l1_block),
            U256::from(l2_timestamp),
            amount,
            data,
        )
        .send()
        .await
    {
        Ok(pending) => {
            let receipt = pending.get_receipt().await?;
            println!("[{}] Epoch {} relayed successfully! tx: {:?}", route_name, epoch, receipt.transaction_hash);
            Ok(())
        }
        Err(e) => {
            eprintln!("[{}] Failed to execute relay for epoch {}: {}", route_name, epoch, e);
            Err(e.into())
        }
    }
}

async fn fetch_outbox_proof(
    arb_provider: DynProvider<Ethereum>,
    position: U256,
) -> Result<Vec<FixedBytes<32>>, Box<dyn std::error::Error + Send + Sync>> {
    let arb_sys = IArbSys::new(ARB_SYS, arb_provider.clone());
    let state = arb_sys.sendMerkleTreeState().call().await?;
    let size = state.size.to::<u64>();

    let node_interface = INodeInterface::new(NODE_INTERFACE, arb_provider);
    let leaf = position.to::<u64>();
    let result = node_interface.constructOutboxProof(size, leaf).call().await?;

    Ok(result.proof)
}
