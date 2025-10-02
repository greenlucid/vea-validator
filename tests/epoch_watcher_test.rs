use alloy::providers::{Provider, ProviderBuilder};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use vea_validator::epoch_watcher::EpochWatcher;

#[tokio::test]
async fn test_epoch_watcher_triggers_handler() {
    dotenv::dotenv().ok();
    
    // Setup provider
    let arbitrum_rpc = std::env::var("ARBITRUM_RPC_URL")
        .expect("ARBITRUM_RPC_URL must be set");
    
    let provider = ProviderBuilder::new()
        .connect_http(arbitrum_rpc.parse().unwrap());
    let provider = Arc::new(provider);
    
    // Create epoch watcher
    let watcher = EpochWatcher::new(provider.clone());
    
    // Track if handler was called
    let handler_called = Arc::new(AtomicBool::new(false));
    let handler_called_clone = handler_called.clone();
    
    // Start watching epochs in background with 10 second period
    let watch_handle = tokio::spawn(async move {
        watcher.watch_epochs(10, move |epoch| {
            let handler_called = handler_called_clone.clone();
            Box::pin(async move {
                println!("Epoch handler triggered for epoch {}", epoch);
                handler_called.store(true, Ordering::Relaxed);
                Ok(())
            })
        }).await
    });
    
    // Advance time to trigger epoch change
    let advance_time = 15u64; // Advance 15 seconds to ensure epoch change
    let _: serde_json::Value = provider
        .raw_request("evm_increaseTime".into(), vec![serde_json::json!(advance_time)])
        .await
        .expect("Failed to advance time");
    
    // Mine a block to apply the time change
    let empty_params: Vec<serde_json::Value> = vec![];
    let _: serde_json::Value = provider
        .raw_request("evm_mine".into(), empty_params)
        .await
        .expect("Failed to mine block");
    
    // Poll for handler to be called (max 2 seconds)
    for _ in 0..20 {
        if handler_called.load(Ordering::Relaxed) {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    
    assert!(
        handler_called.load(Ordering::Relaxed),
        "Handler should have been called when epoch changed"
    );
    
    // Stop the watcher
    watch_handle.abort();
}