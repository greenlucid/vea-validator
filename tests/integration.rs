use alloy::providers::{Provider, ProviderBuilder};
use alloy::primitives::Address;
use alloy::rpc::types::Filter;
use alloy::sol;
use alloy::sol_types::SolEvent;
use std::str::FromStr;

sol! {
    event SnapshotSaved(bytes32 _snapshot, uint256 _epoch, uint64 _count);
}

#[tokio::test]
async fn test_listen_snapshot_saved_event() {
    dotenv::dotenv().ok();
    
    let arbitrum_rpc = std::env::var("ARBITRUM_RPC_URL")
        .expect("ARBITRUM_RPC_URL must be set");
    
    let vea_inbox_address = std::env::var("VEA_INBOX_ARB_TO_ETH")
        .expect("VEA_INBOX_ARB_TO_ETH must be set");
    
    let provider = ProviderBuilder::new()
        .connect_http(arbitrum_rpc.parse().unwrap());
    
    let inbox_address = Address::from_str(&vea_inbox_address).unwrap();
    
    let filter = Filter::new()
        .address(inbox_address)
        .event_signature(SnapshotSaved::SIGNATURE_HASH);
    
    let _logs = provider.get_logs(&filter).await.unwrap();
    
    // Test passes if we can query (doesn't matter if logs exist)
    assert!(true, "Query succeeded");
}