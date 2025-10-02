use alloy::primitives::Address;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::network::EthereumWallet;
use alloy::signers::local::PrivateKeySigner;
use std::sync::Arc;
use std::str::FromStr;
use vea_validator::event_listener::EventListener;
use vea_validator::contracts::IVeaInboxArbToEth;
#[allow(unused_imports)]
use serial_test::serial;

// Test constants
const TEST_PRIVATE_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

// Common test setup
fn setup_test() {
    dotenv::dotenv().ok();
}

// Test fixture for managing blockchain state snapshots
struct TestFixture<P: Provider> {
    provider: Arc<P>,
    snapshot_id: Option<String>,
}

impl<P: Provider> TestFixture<P> {
    fn new(provider: Arc<P>) -> Self {
        Self {
            provider,
            snapshot_id: None,
        }
    }

    async fn take_snapshot(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let empty_params: Vec<serde_json::Value> = vec![];
        let snapshot_result: serde_json::Value = self.provider
            .raw_request("evm_snapshot".into(), empty_params)
            .await?;
        
        self.snapshot_id = Some(snapshot_result.as_str().unwrap().to_string());
        println!("Created snapshot: {}", self.snapshot_id.as_ref().unwrap());
        Ok(())
    }

    async fn revert_snapshot(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(ref snapshot_id) = self.snapshot_id {
            let _: serde_json::Value = self.provider
                .raw_request("evm_revert".into(), vec![serde_json::json!(snapshot_id)])
                .await?;
            println!("Reverted to snapshot: {}", snapshot_id);
        }
        Ok(())
    }

}

// Helper functions
fn get_contract_addresses() -> (Address, Address) {
    let inbox_address = Address::from_str(
        &std::env::var("VEA_INBOX_ARB_TO_ETH")
            .expect("VEA_INBOX_ARB_TO_ETH must be set")
    ).expect("Invalid inbox address");
    
    let outbox_address = Address::from_str(
        &std::env::var("VEA_OUTBOX_ARB_TO_ETH")
            .expect("VEA_OUTBOX_ARB_TO_ETH must be set")
    ).expect("Invalid outbox address");
    
    (inbox_address, outbox_address)
}

fn create_signer_wallet() -> EthereumWallet {
    let signer = PrivateKeySigner::from_str(TEST_PRIVATE_KEY).unwrap();
    EthereumWallet::from(signer)
}

#[tokio::test]
#[serial]
async fn test_watch_snapshot_events() {
    setup_test();
    
    // Setup provider
    let arbitrum_rpc = std::env::var("ARBITRUM_RPC_URL")
        .expect("ARBITRUM_RPC_URL must be set");
    
    let provider = ProviderBuilder::new()
        .connect_http(arbitrum_rpc.parse().unwrap());
    let provider = Arc::new(provider);
    
    // Create test fixture and take snapshot
    let mut fixture = TestFixture::new(provider.clone());
    fixture.take_snapshot().await.unwrap();
    
    // Get contract addresses
    let (inbox_address, _outbox_address) = get_contract_addresses();
    
    // Create event listener
    let listener = EventListener::new(provider.clone(), inbox_address);
    
    // Setup a flag to track if handler was called
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    
    // Start watching in background
    let watch_handle = tokio::spawn(async move {
        listener.watch_snapshots(move |event| {
            let tx = tx.clone();
            Box::pin(async move {
                println!("Handler triggered for epoch {}", event.epoch);
                tx.try_send(event).ok();
                Ok(())
            })
        }).await
    });
    
    // Give the watcher time to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    
    // Trigger a snapshot event
    trigger_snapshot_saved(provider.clone(), inbox_address).await;
    
    // Wait for the handler to be called
    let timeout = tokio::time::timeout(
        tokio::time::Duration::from_secs(5),
        rx.recv()
    ).await;
    
    // Verify handler was triggered
    assert!(timeout.is_ok(), "Handler should have been triggered within 5 seconds");
    let event = timeout.unwrap().unwrap();
    assert!(event.epoch > 0, "Epoch should be non-zero");
    // Note: count may not be 0 if messages were sent in other tests
    println!("Snapshot event received: epoch={}, count={}", event.epoch, event.count);
    
    // Stop the watcher
    watch_handle.abort();
    
    // Cleanup: revert to snapshot
    fixture.revert_snapshot().await.unwrap();
}


// Helper function to trigger a snapshot event
async fn trigger_snapshot_saved<P: Provider>(provider: Arc<P>, inbox_address: Address) {
    let wallet = create_signer_wallet();
    let provider_with_wallet = ProviderBuilder::new()
        .wallet(wallet)
        .connect_provider(&*provider);
    
    let contract = IVeaInboxArbToEth::new(inbox_address, provider_with_wallet);
    
    println!("Calling saveSnapshot on {}", inbox_address);
    let tx = contract.saveSnapshot();
    let receipt = tx.send().await.unwrap().get_receipt().await.unwrap();
    println!("Transaction successful: {:?}", receipt.transaction_hash);
}


