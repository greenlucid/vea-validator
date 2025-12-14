use alloy::primitives::{Address, FixedBytes, U256};
use alloy::providers::DynProvider;
use alloy::network::Ethereum;
use crate::contracts::{IVeaOutboxArbToEth, IVeaOutboxArbToGnosis, Claim, Party};

pub async fn execute(
    outbox_provider: DynProvider<Ethereum>,
    outbox_address: Address,
    weth_address: Option<Address>,
    wallet_address: Address,
    epoch: u64,
    state_root: FixedBytes<32>,
    claimer: Address,
    timestamp_claimed: u32,
    route_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("[{}] Calling challenge for epoch {}", route_name, epoch);

    let claim = Claim {
        stateRoot: state_root,
        claimer,
        timestampClaimed: timestamp_claimed,
        timestampVerification: 0,
        blocknumberVerification: 0,
        honest: Party::None,
        challenger: Address::ZERO,
    };

    if weth_address.is_some() {
        let outbox = IVeaOutboxArbToGnosis::new(outbox_address, outbox_provider);
        match outbox.challenge(U256::from(epoch), claim).send().await {
            Ok(pending) => {
                let receipt = pending.get_receipt().await?;
                if !receipt.status() {
                    panic!("[{}] FATAL: challenge reverted for epoch {}", route_name, epoch);
                }
                println!("[{}] challenge succeeded for epoch {}", route_name, epoch);
                Ok(())
            }
            Err(e) => {
                let err_msg = e.to_string();
                if err_msg.contains("Invalid claim") || err_msg.contains("already") {
                    println!("[{}] Claim already challenged - bridge is safe", route_name);
                    return Ok(());
                }
                panic!("[{}] FATAL: challenge failed for epoch {}: {}", route_name, epoch, e);
            }
        }
    } else {
        let outbox = IVeaOutboxArbToEth::new(outbox_address, outbox_provider);
        let deposit = outbox.deposit().call().await
            .map_err(|e| panic!("[{}] FATAL: Failed to get deposit for challenge: {}", route_name, e))?;

        match outbox.challenge(U256::from(epoch), claim, wallet_address).value(deposit).send().await {
            Ok(pending) => {
                let receipt = pending.get_receipt().await?;
                if !receipt.status() {
                    panic!("[{}] FATAL: challenge reverted for epoch {}", route_name, epoch);
                }
                println!("[{}] challenge succeeded for epoch {}", route_name, epoch);
                Ok(())
            }
            Err(e) => {
                let err_msg = e.to_string();
                if err_msg.contains("Invalid claim") || err_msg.contains("already") {
                    println!("[{}] Claim already challenged - bridge is safe", route_name);
                    return Ok(());
                }
                panic!("[{}] FATAL: challenge failed for epoch {}: {}", route_name, epoch, e);
            }
        }
    }
}
