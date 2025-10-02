use alloy::primitives::Address;
use alloy::providers::ProviderBuilder;
use alloy::signers::local::PrivateKeySigner;
use alloy::network::{Ethereum, EthereumWallet};
use std::str::FromStr;
use std::sync::Arc;
use vea_validator::{
    event_listener::{EventListener, SnapshotEvent, ClaimEvent},
    epoch_watcher::EpochWatcher,
    claim_handler::{ClaimHandler, ClaimAction},
    contracts::{IVeaInboxArbToEth, IVeaOutboxArbToEth},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv::dotenv().ok();
    
    let route = std::env::var("VEA_ROUTE").expect("VEA_ROUTE must be set (ARB_TO_ETH or ARB_TO_GNOSIS)");
    
    let (inbox_address, outbox_address, destination_rpc) = match route.as_str() {
        "ARB_TO_ETH" => {
            let inbox = Address::from_str(
                &std::env::var("VEA_INBOX_ARB_TO_ETH")
                    .expect("VEA_INBOX_ARB_TO_ETH must be set")
            )?;
            let outbox = Address::from_str(
                &std::env::var("VEA_OUTBOX_ARB_TO_ETH")
                    .expect("VEA_OUTBOX_ARB_TO_ETH must be set")
            )?;
            let rpc = std::env::var("ETHEREUM_RPC_URL")
                .or_else(|_| std::env::var("MAINNET_RPC_URL"))
                .expect("ETHEREUM_RPC_URL or MAINNET_RPC_URL must be set");
            (inbox, outbox, rpc)
        },
        "ARB_TO_GNOSIS" => {
            let inbox = Address::from_str(
                &std::env::var("VEA_INBOX_ARB_TO_GNOSIS")
                    .expect("VEA_INBOX_ARB_TO_GNOSIS must be set")
            )?;
            let outbox = Address::from_str(
                &std::env::var("VEA_OUTBOX_ARB_TO_GNOSIS")
                    .expect("VEA_OUTBOX_ARB_TO_GNOSIS must be set")
            )?;
            let rpc = std::env::var("GNOSIS_RPC_URL")
                .expect("GNOSIS_RPC_URL must be set");
            (inbox, outbox, rpc)
        },
        _ => {
            panic!("Invalid route: {}. Must be ARB_TO_ETH or ARB_TO_GNOSIS", route);
        }
    };
    
    let arbitrum_rpc = std::env::var("ARBITRUM_RPC_URL")
        .expect("ARBITRUM_RPC_URL must be set");
    
    let private_key = if let Ok(key) = std::env::var("PRIVATE_KEY") {
        key
    } else if let Ok(_) = std::env::var("PRIVATE_KEY_FILE") {
        std::fs::read_to_string("/run/secrets/validator_key")
            .expect("Failed to read private key from Docker secret")
            .trim()
            .to_string()
    } else {
        panic!("PRIVATE_KEY not provided. Set via environment variable or Docker secret");
    };
    
    let destination_provider = ProviderBuilder::new()
        .connect_http(destination_rpc.parse()?);
    let destination_provider = Arc::new(destination_provider);
    
    let arbitrum_provider = ProviderBuilder::new()
        .connect_http(arbitrum_rpc.parse()?);
    let arbitrum_provider = Arc::new(arbitrum_provider);
    
    let signer = PrivateKeySigner::from_str(&private_key)?;
    let wallet_address = signer.address();
    let wallet = EthereumWallet::from(signer);
    
    let destination_provider_with_wallet = ProviderBuilder::<_, _, Ethereum>::new()
        .wallet(wallet.clone())
        .connect_provider(destination_provider.clone());
    let destination_provider_with_wallet = Arc::new(destination_provider_with_wallet);
    
    let arbitrum_provider_with_wallet = ProviderBuilder::<_, _, Ethereum>::new()
        .wallet(wallet)
        .connect_provider(arbitrum_provider.clone());
    let arbitrum_provider_with_wallet = Arc::new(arbitrum_provider_with_wallet);
    
    let claim_handler = Arc::new(ClaimHandler::new(
        destination_provider_with_wallet.clone(),
        arbitrum_provider_with_wallet.clone(),
        outbox_address,
        inbox_address,
        wallet_address,
    ));
    
    let event_listener_inbox = EventListener::new(
        arbitrum_provider.clone(),
        inbox_address,
    );
    
    let event_listener_outbox = EventListener::new(
        destination_provider.clone(),
        outbox_address,
    );
    
    let epoch_watcher = EpochWatcher::new(
        arbitrum_provider.clone(),
    );
    
    let inbox_contract = IVeaInboxArbToEth::new(inbox_address, arbitrum_provider.clone());
    let epoch_period: u64 = inbox_contract.epochPeriod().call().await?.try_into()?;
    
    let _current_epoch: u64 = inbox_contract.epochFinalized().call().await?.try_into()?;
    
    let claim_handler_for_epoch = claim_handler.clone();
    let epoch_handle = tokio::spawn(async move {
        epoch_watcher.watch_epochs(epoch_period, move |epoch| {
            let handler = claim_handler_for_epoch.clone();
            Box::pin(async move {
                match handler.handle_epoch_end(epoch).await {
                    Ok(action) => {
                        match action {
                            ClaimAction::None => {
                            }
                            ClaimAction::Claim { epoch, state_root } => {
                                let _ = handler.submit_claim(epoch, state_root).await;
                            }
                            ClaimAction::Challenge { epoch, incorrect_claim } => {
                                let claim = IVeaOutboxArbToEth::Claim {
                                    stateRoot: incorrect_claim.state_root,
                                    claimer: incorrect_claim.claimer,
                                    timestampClaimed: 0,
                                    timestampVerification: 0,
                                    blocknumberVerification: 0,
                                    honest: IVeaOutboxArbToEth::Party::None,
                                    challenger: Address::ZERO,
                                };
                                let _ = handler.challenge_claim(epoch, claim).await;
                            }
                        }
                    }
                    Err(_) => {},
                }
                Ok(())
            })
        }).await
    });
    
    let claim_handler_for_snapshots = claim_handler.clone();
    let snapshot_handle = tokio::spawn(async move {
        event_listener_inbox.watch_snapshots(move |event: SnapshotEvent| {
            let handler = claim_handler_for_snapshots.clone();
            Box::pin(async move {
                println!("Snapshot saved for epoch {} with root {:?}", event.epoch, event.state_root);
                
                match handler.get_claim_for_epoch(event.epoch).await {
                    Ok(Some(existing_claim)) => {
                        println!("Claim already exists for epoch {}", event.epoch);
                        // Verify it's correct
                        match handler.verify_claim(&existing_claim).await {
                            Ok(true) => println!("Existing claim is valid"),
                            Ok(false) => {
                                println!("Existing claim is INVALID - need to challenge");
                                // Convert ClaimEvent to Claim struct
                                let claim = IVeaOutboxArbToEth::Claim {
                                    stateRoot: existing_claim.state_root,
                                    claimer: existing_claim.claimer,
                                    timestampClaimed: 0, // Will be set by contract
                                    timestampVerification: 0,
                                    blocknumberVerification: 0,
                                    honest: IVeaOutboxArbToEth::Party::None,
                                    challenger: Address::ZERO,
                                };
                                if let Err(e) = handler.challenge_claim(event.epoch, claim).await {
                                    eprintln!("Failed to challenge claim: {}", e);
                                }
                            }
                            Err(e) => eprintln!("Error verifying claim: {}", e),
                        }
                    }
                    Ok(None) => {
                        println!("No claim for epoch {} - submitting claim", event.epoch);
                        if let Err(e) = handler.submit_claim(event.epoch, event.state_root).await {
                            eprintln!("Failed to submit claim: {}", e);
                        }
                    }
                    Err(e) => eprintln!("Error checking claim: {}", e),
                }
                Ok(())
            })
        }).await
    });
    
    // Watch for claim events on outbox
    let claim_handler_for_claims = claim_handler.clone();
    let claim_handle = tokio::spawn(async move {
        event_listener_outbox.watch_claims(move |event: ClaimEvent| {
            let handler = claim_handler_for_claims.clone();
            Box::pin(async move {
                println!("Claim detected for epoch {} by {}", event.epoch, event.claimer);
                
                match handler.handle_claim_event(event.clone()).await {
                    Ok(ClaimAction::Challenge { epoch, incorrect_claim }) => {
                        println!("Need to challenge incorrect claim for epoch {}", epoch);
                        // The event contains the claim data
                        let claim = IVeaOutboxArbToEth::Claim {
                            stateRoot: incorrect_claim.state_root,
                            claimer: incorrect_claim.claimer,
                            timestampClaimed: 0, // Will be set by contract
                            timestampVerification: 0,
                            blocknumberVerification: 0,
                            honest: IVeaOutboxArbToEth::Party::None,
                            challenger: Address::ZERO,
                        };
                        if let Err(e) = handler.challenge_claim(epoch, claim).await {
                            eprintln!("Failed to challenge claim: {}", e);
                        }
                    }
                    Ok(_) => {
                        println!("Claim is valid or no action needed");
                    }
                    Err(e) => eprintln!("Error handling claim event: {}", e),
                }
                Ok(())
            })
        }).await
    });
    
    // Running
    
    // Wait for all tasks
    tokio::select! {
        _ = epoch_handle => println!("Epoch watcher stopped"),
        _ = snapshot_handle => println!("Snapshot watcher stopped"),
        _ = claim_handle => println!("Claim watcher stopped"),
        _ = tokio::signal::ctrl_c() => println!("\nShutting down..."),
    }
    
    Ok(())
}