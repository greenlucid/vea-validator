use alloy::primitives::{Address, FixedBytes, U256};
use alloy::providers::DynProvider;
use alloy::network::Ethereum;
use crate::contracts::{IVeaOutbox, Claim, Party};

pub async fn execute(
    outbox_provider: DynProvider<Ethereum>,
    outbox_address: Address,
    epoch: u64,
    state_root: FixedBytes<32>,
    claimer: Address,
    timestamp_claimed: u32,
    timestamp_verification: u32,
    blocknumber_verification: u32,
    route_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("[{}] Calling verifySnapshot for epoch {}", route_name, epoch);

    let claim = Claim {
        stateRoot: state_root,
        claimer,
        timestampClaimed: timestamp_claimed,
        timestampVerification: timestamp_verification,
        blocknumberVerification: blocknumber_verification,
        honest: Party::None,
        challenger: Address::ZERO,
    };

    let outbox = IVeaOutbox::new(outbox_address, outbox_provider);
    match outbox.verifySnapshot(U256::from(epoch), claim).send().await {
        Ok(pending) => {
            let receipt = pending.get_receipt().await?;
            if !receipt.status() {
                eprintln!("[{}] verifySnapshot reverted for epoch {}", route_name, epoch);
                return Err("verifySnapshot reverted".into());
            }
            println!("[{}] verifySnapshot succeeded for epoch {}", route_name, epoch);
            Ok(())
        }
        Err(e) => {
            let err_msg = e.to_string();
            if err_msg.contains("already") {
                println!("[{}] verifySnapshot already done for epoch {}", route_name, epoch);
                return Ok(());
            }
            eprintln!("[{}] verifySnapshot failed for epoch {}: {}", route_name, epoch, e);
            Err(e.into())
        }
    }
}
