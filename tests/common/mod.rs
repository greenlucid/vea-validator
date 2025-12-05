pub use alloy::providers::Provider;
use alloy::providers::ProviderBuilder;

const PRISTINE_SNAPSHOT: &str = "0x0";

/// Restores all devnet chains to pristine state. Call at START of each test.
pub async fn restore_pristine() {
    let arb = ProviderBuilder::new().connect_http("http://localhost:8545".parse().unwrap());
    let eth = ProviderBuilder::new().connect_http("http://localhost:8546".parse().unwrap());
    let gnosis = ProviderBuilder::new().connect_http("http://localhost:8547".parse().unwrap());

    let _: serde_json::Value = arb.raw_request("evm_revert".into(), vec![serde_json::json!(PRISTINE_SNAPSHOT)]).await.unwrap();
    let _: serde_json::Value = eth.raw_request("evm_revert".into(), vec![serde_json::json!(PRISTINE_SNAPSHOT)]).await.unwrap();
    let _: serde_json::Value = gnosis.raw_request("evm_revert".into(), vec![serde_json::json!(PRISTINE_SNAPSHOT)]).await.unwrap();
}

pub async fn advance_time<P: Provider>(provider: &P, seconds: u64) {
    let _: serde_json::Value = provider
        .raw_request("evm_increaseTime".into(), vec![serde_json::json!(seconds)])
        .await
        .unwrap();
    let _: serde_json::Value = provider
        .raw_request("evm_mine".into(), Vec::<serde_json::Value>::new())
        .await
        .unwrap();
}
