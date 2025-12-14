use alloy::primitives::{Address, FixedBytes, U256};
use alloy::providers::DynProvider;
use alloy::network::Ethereum;
use crate::contracts::{IVeaInboxArbToEth, IVeaInboxArbToGnosis, Claim, Party};

pub async fn execute(
    inbox_provider: DynProvider<Ethereum>,
    inbox_address: Address,
    weth_address: Option<Address>,
    epoch: u64,
    state_root: FixedBytes<32>,
    claimer: Address,
    timestamp_claimed: u32,
    challenger: Address,
    route_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("[{}] Calling sendSnapshot for epoch {}", route_name, epoch);

    let claim = Claim {
        stateRoot: state_root,
        claimer,
        timestampClaimed: timestamp_claimed,
        timestampVerification: 0,
        blocknumberVerification: 0,
        honest: Party::None,
        challenger,
    };

    if weth_address.is_some() {
        let inbox = IVeaInboxArbToGnosis::new(inbox_address, inbox_provider);
        let gas_limit = U256::from(500000);
        match inbox.sendSnapshot(U256::from(epoch), gas_limit, claim).send().await {
            Ok(pending) => {
                let receipt = pending.get_receipt().await?;
                if !receipt.status() {
                    panic!("[{}] FATAL: sendSnapshot reverted for epoch {}", route_name, epoch);
                }
                println!("[{}] sendSnapshot succeeded for epoch {}", route_name, epoch);
                Ok(())
            }
            Err(e) => {
                panic!("[{}] FATAL: sendSnapshot failed for epoch {}: {}", route_name, epoch, e);
            }
        }
    } else {
        let inbox = IVeaInboxArbToEth::new(inbox_address, inbox_provider);
        match inbox.sendSnapshot(U256::from(epoch), claim).send().await {
            Ok(pending) => {
                let receipt = pending.get_receipt().await?;
                if !receipt.status() {
                    panic!("[{}] FATAL: sendSnapshot reverted for epoch {}", route_name, epoch);
                }
                println!("[{}] sendSnapshot succeeded for epoch {}", route_name, epoch);
                Ok(())
            }
            Err(e) => {
                panic!("[{}] FATAL: sendSnapshot failed for epoch {}: {}", route_name, epoch, e);
            }
        }
    }
}
