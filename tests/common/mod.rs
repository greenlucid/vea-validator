pub use alloy::providers::Provider;
use alloy::providers::ProviderBuilder;

const SNAPSHOT_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/.devnet-snapshot");

pub async fn restore_pristine() {
    let snapshot_id = std::fs::read_to_string(SNAPSHOT_FILE)
        .unwrap_or_else(|_| panic!("Missing {}. Run full-devnet.sh first.", SNAPSHOT_FILE));
    let snapshot_id = snapshot_id.trim();

    let arb = ProviderBuilder::new().connect_http("http://localhost:8545".parse().unwrap());
    let eth = ProviderBuilder::new().connect_http("http://localhost:8546".parse().unwrap());
    let gnosis = ProviderBuilder::new().connect_http("http://localhost:8547".parse().unwrap());

    let _: serde_json::Value = arb.raw_request("evm_revert".into(), vec![serde_json::json!(snapshot_id)]).await.unwrap();
    let _: serde_json::Value = eth.raw_request("evm_revert".into(), vec![serde_json::json!(snapshot_id)]).await.unwrap();
    let _: serde_json::Value = gnosis.raw_request("evm_revert".into(), vec![serde_json::json!(snapshot_id)]).await.unwrap();

    let new_id: serde_json::Value = arb.raw_request("evm_snapshot".into(), Vec::<serde_json::Value>::new()).await.unwrap();
    let _: serde_json::Value = eth.raw_request("evm_snapshot".into(), Vec::<serde_json::Value>::new()).await.unwrap();
    let _: serde_json::Value = gnosis.raw_request("evm_snapshot".into(), Vec::<serde_json::Value>::new()).await.unwrap();

    std::fs::write(SNAPSHOT_FILE, new_id.as_str().unwrap()).unwrap();
}

pub async fn advance_time(seconds: u64) {
    let arb = ProviderBuilder::new().connect_http("http://localhost:8545".parse().unwrap());
    let eth = ProviderBuilder::new().connect_http("http://localhost:8546".parse().unwrap());
    let gnosis = ProviderBuilder::new().connect_http("http://localhost:8547".parse().unwrap());

    for p in [&arb, &eth, &gnosis] {
        let _: serde_json::Value = p
            .raw_request("anvil_mine".into(), vec![serde_json::json!(1), serde_json::json!(seconds)])
            .await
            .unwrap();
    }
}
