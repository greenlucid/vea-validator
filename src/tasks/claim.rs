use alloy::primitives::{Address, FixedBytes, U256};
use alloy::providers::DynProvider;
use alloy::network::Ethereum;
use crate::contracts::{IVeaInboxArbToEth, IVeaOutboxArbToEth};

pub async fn execute(
    inbox_provider: DynProvider<Ethereum>,
    inbox_address: Address,
    outbox_provider: DynProvider<Ethereum>,
    outbox_address: Address,
    epoch: u64,
    route_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let inbox = IVeaInboxArbToEth::new(inbox_address, inbox_provider);
    let outbox = IVeaOutboxArbToEth::new(outbox_address, outbox_provider);

    let state_root = inbox.snapshots(U256::from(epoch)).call().await?;
    if state_root == FixedBytes::<32>::ZERO {
        println!("[{}] No snapshot for epoch {}, skipping claim", route_name, epoch);
        return Ok(());
    }

    let claim_hash = outbox.claimHashes(U256::from(epoch)).call().await?;
    if claim_hash != FixedBytes::<32>::ZERO {
        println!("[{}] Claim already exists for epoch {}", route_name, epoch);
        return Ok(());
    }

    let deposit = outbox.deposit().call().await?;
    let tx = outbox.claim(U256::from(epoch), state_root).value(deposit);

    match tx.send().await {
        Ok(pending) => {
            let receipt = pending.get_receipt().await?;
            if !receipt.status() {
                panic!("[{}] FATAL: Claim transaction reverted for epoch {}", route_name, epoch);
            }
            println!("[{}] Claim succeeded for epoch {}", route_name, epoch);
            Ok(())
        }
        Err(e) => {
            let err_msg = e.to_string();
            if err_msg.contains("Claim already made") {
                println!("[{}] Claim already made by another validator for epoch {} - bridge is safe", route_name, epoch);
                return Ok(());
            }
            panic!("[{}] FATAL: Unexpected error submitting claim for epoch {}: {}", route_name, epoch, e);
        }
    }
}
