use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::network::{Ethereum, EthereumWallet};
use std::str::FromStr;
use std::sync::Arc;
use vea_validator::{
    event_listener::EventListener,
    claim_handler::{ClaimHandler, ClaimAction},
    contracts::{IVeaInboxArbToEth, IVeaOutboxArbToEth},
};

// Test fixture for managing blockchain state snapshots
struct TestFixture<P1: Provider, P2: Provider> {
    eth_provider: Arc<P1>,
    arb_provider: Arc<P2>,
    eth_snapshot_id: Option<String>,
    arb_snapshot_id: Option<String>,
}

impl<P1: Provider, P2: Provider> TestFixture<P1, P2> {
    fn new(eth_provider: Arc<P1>, arb_provider: Arc<P2>) -> Self {
        Self {
            eth_provider,
            arb_provider,
            eth_snapshot_id: None,
            arb_snapshot_id: None,
        }
    }

    async fn take_snapshots(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let empty_params: Vec<serde_json::Value> = vec![];
        
        let eth_snapshot: serde_json::Value = self.eth_provider
            .raw_request("evm_snapshot".into(), empty_params.clone())
            .await?;
        self.eth_snapshot_id = Some(eth_snapshot.as_str().unwrap().to_string());
        
        let arb_snapshot: serde_json::Value = self.arb_provider
            .raw_request("evm_snapshot".into(), empty_params)
            .await?;
        self.arb_snapshot_id = Some(arb_snapshot.as_str().unwrap().to_string());
        
        Ok(())
    }

    async fn revert_snapshots(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(ref snapshot_id) = self.eth_snapshot_id {
            let _: serde_json::Value = self.eth_provider
                .raw_request("evm_revert".into(), vec![serde_json::json!(snapshot_id)])
                .await?;
        }
        
        if let Some(ref snapshot_id) = self.arb_snapshot_id {
            let _: serde_json::Value = self.arb_provider
                .raw_request("evm_revert".into(), vec![serde_json::json!(snapshot_id)])
                .await?;
        }
        
        Ok(())
    }
}

#[tokio::test]
async fn test_full_validator_flow() {
    dotenv::dotenv().ok();
    
    // Load configuration
    let inbox_address = Address::from_str(
        &std::env::var("VEA_INBOX_ARB_TO_ETH")
            .expect("VEA_INBOX_ARB_TO_ETH must be set")
    ).expect("Invalid inbox address");
    
    let outbox_address = Address::from_str(
        &std::env::var("VEA_OUTBOX_ARB_TO_ETH")
            .expect("VEA_OUTBOX_ARB_TO_ETH must be set")
    ).expect("Invalid outbox address");
    
    let arbitrum_rpc = std::env::var("ARBITRUM_RPC_URL")
        .expect("ARBITRUM_RPC_URL must be set");
    
    let ethereum_rpc = std::env::var("ETHEREUM_RPC_URL")
        .or_else(|_| std::env::var("MAINNET_RPC_URL"))
        .expect("ETHEREUM_RPC_URL or MAINNET_RPC_URL must be set");
    
    let private_key = std::env::var("PRIVATE_KEY")
        .unwrap_or_else(|_| "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80".to_string());
    
    // Setup providers
    let ethereum_provider = ProviderBuilder::new()
        .connect_http(ethereum_rpc.parse().unwrap());
    let ethereum_provider = Arc::new(ethereum_provider);
    
    let arbitrum_provider = ProviderBuilder::new()
        .connect_http(arbitrum_rpc.parse().unwrap());
    let arbitrum_provider = Arc::new(arbitrum_provider);
    
    // Setup wallet
    let signer = PrivateKeySigner::from_str(&private_key).unwrap();
    let wallet_address = signer.address();
    let wallet = EthereumWallet::from(signer);
    
    // Create providers with wallet
    let ethereum_provider_with_wallet = ProviderBuilder::<_, _, Ethereum>::new()
        .wallet(wallet.clone())
        .connect_provider(ethereum_provider.clone());
    let ethereum_provider_with_wallet = Arc::new(ethereum_provider_with_wallet);
    
    let arbitrum_provider_with_wallet = ProviderBuilder::<_, _, Ethereum>::new()
        .wallet(wallet)
        .connect_provider(arbitrum_provider.clone());
    let arbitrum_provider_with_wallet = Arc::new(arbitrum_provider_with_wallet);
    
    // Create test fixture and take snapshots
    let mut fixture = TestFixture::new(
        ethereum_provider.clone(),
        arbitrum_provider.clone()
    );
    fixture.take_snapshots().await.unwrap();
    
    // Create claim handler
    let claim_handler = ClaimHandler::new(
        ethereum_provider_with_wallet.clone(),
        arbitrum_provider_with_wallet.clone(),
        outbox_address,
        inbox_address,
        wallet_address,
    );
    
    // Test 1: Check contract connectivity
    println!("Test 1: Checking contract connectivity...");
    let inbox_contract = IVeaInboxArbToEth::new(inbox_address, arbitrum_provider.clone());
    let epoch_period = inbox_contract.epochPeriod().call().await.unwrap();
    println!("Epoch period: {}", epoch_period);
    assert!(epoch_period > U256::ZERO, "Epoch period should be non-zero");
    
    // Test 2: Get current epoch
    println!("Test 2: Getting current epoch...");
    let current_epoch: u64 = inbox_contract.epochNow().call().await.unwrap().try_into().unwrap();
    println!("Current epoch: {}", current_epoch);
    
    // Test 3: Check if we can query snapshots
    println!("Test 3: Checking snapshot query...");
    let snapshot = inbox_contract.snapshots(U256::from(current_epoch.saturating_sub(1))).call().await.unwrap();
    println!("Snapshot for previous epoch: {:?}", snapshot);
    
    // Test 4: Check outbox claim hash
    println!("Test 4: Checking outbox claim hash...");
    let outbox_contract = IVeaOutboxArbToEth::new(outbox_address, ethereum_provider.clone());
    let claim_hash = outbox_contract.claimHashes(U256::from(current_epoch.saturating_sub(1))).call().await.unwrap();
    println!("Claim hash for previous epoch: {:?}", claim_hash);
    
    // Test 5: Test claim handler functions
    println!("Test 5: Testing claim handler...");
    
    // Check if claim exists
    let existing_claim = claim_handler.get_claim_for_epoch(current_epoch.saturating_sub(1)).await.unwrap();
    println!("Existing claim: {:?}", existing_claim);
    
    // Get correct state root
    let correct_root = claim_handler.get_correct_state_root(current_epoch.saturating_sub(1)).await.unwrap();
    println!("Correct state root: {:?}", correct_root);
    
    // Test 6: Verify claim action logic
    println!("Test 6: Testing claim action logic...");
    let action = claim_handler.handle_epoch_end(current_epoch.saturating_sub(1)).await.unwrap();
    match action {
        ClaimAction::None => println!("No action needed"),
        ClaimAction::Claim { epoch, state_root } => {
            println!("Would claim epoch {} with root {:?}", epoch, state_root);
        }
        ClaimAction::Challenge { epoch, .. } => {
            println!("Would challenge epoch {}", epoch);
        }
    }
    
    println!("All integration tests passed!");
    
    // Revert blockchain state
    fixture.revert_snapshots().await.unwrap();
}

#[tokio::test]
async fn test_event_watching() {
    dotenv::dotenv().ok();
    
    let arbitrum_rpc = std::env::var("ARBITRUM_RPC_URL")
        .expect("ARBITRUM_RPC_URL must be set");
    
    let ethereum_rpc = std::env::var("ETHEREUM_RPC_URL")
        .or_else(|_| std::env::var("MAINNET_RPC_URL"))
        .expect("ETHEREUM_RPC_URL or MAINNET_RPC_URL must be set");
    
    let inbox_address = Address::from_str(
        &std::env::var("VEA_INBOX_ARB_TO_ETH")
            .expect("VEA_INBOX_ARB_TO_ETH must be set")
    ).expect("Invalid inbox address");
    
    let provider = ProviderBuilder::new()
        .connect_http(arbitrum_rpc.parse().unwrap());
    let provider = Arc::new(provider);
    
    let ethereum_provider = ProviderBuilder::new()
        .connect_http(ethereum_rpc.parse().unwrap());
    let ethereum_provider = Arc::new(ethereum_provider);
    
    // Create test fixture and take snapshots
    let mut fixture = TestFixture::new(
        ethereum_provider,
        provider.clone()
    );
    fixture.take_snapshots().await.unwrap();
    
    let event_listener = EventListener::new(provider.clone(), inbox_address);
    
    // Start watching in a spawned task
    let handle = tokio::spawn(async move {
        event_listener.watch_snapshots(|event| {
            Box::pin(async move {
                println!("Snapshot event detected: epoch {} with root {:?}", 
                    event.epoch, event.state_root);
                Ok(())
            })
        }).await
    });
    
    // Let it run briefly to verify it starts correctly
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    
    // Cancel the task
    handle.abort();
    println!("Event watching test completed");
    
    // Revert blockchain state
    fixture.revert_snapshots().await.unwrap();
}