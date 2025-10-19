use alloy::primitives::Address;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::network::{Ethereum, EthereumWallet};
use std::str::FromStr;
use std::sync::Arc;
pub struct ValidatorConfig {
    pub arbitrum_rpc: String,
    pub ethereum_rpc: String,
    pub gnosis_rpc: String,
    pub private_key: String,
    pub inbox_arb_to_eth: Address,
    pub outbox_arb_to_eth: Address,
    pub inbox_arb_to_gnosis: Address,
    pub outbox_arb_to_gnosis: Address,
    pub weth_gnosis: Address,
}
impl ValidatorConfig {
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        dotenv::dotenv().ok();
        let arbitrum_rpc = std::env::var("ARBITRUM_RPC_URL")
            .expect("ARBITRUM_RPC_URL must be set");
        let ethereum_rpc = std::env::var("ETHEREUM_RPC_URL")
            .or_else(|_| std::env::var("MAINNET_RPC_URL"))
            .expect("ETHEREUM_RPC_URL or MAINNET_RPC_URL must be set");
        let gnosis_rpc = std::env::var("GNOSIS_RPC_URL")
            .expect("GNOSIS_RPC_URL must be set");
        let private_key = std::env::var("PRIVATE_KEY")
            .or_else(|_| std::fs::read_to_string("/run/secrets/validator_key")
                .map(|s| s.trim().to_string()))
            .expect("PRIVATE_KEY not set or /run/secrets/validator_key not found");
        let inbox_arb_to_eth = Address::from_str(
            &std::env::var("VEA_INBOX_ARB_TO_ETH")
                .expect("VEA_INBOX_ARB_TO_ETH must be set")
        )?;
        let outbox_arb_to_eth = Address::from_str(
            &std::env::var("VEA_OUTBOX_ARB_TO_ETH")
                .expect("VEA_OUTBOX_ARB_TO_ETH must be set")
        )?;
        let inbox_arb_to_gnosis = Address::from_str(
            &std::env::var("VEA_INBOX_ARB_TO_GNOSIS")
                .expect("VEA_INBOX_ARB_TO_GNOSIS must be set")
        )?;
        let outbox_arb_to_gnosis = Address::from_str(
            &std::env::var("VEA_OUTBOX_ARB_TO_GNOSIS")
                .expect("VEA_OUTBOX_ARB_TO_GNOSIS must be set")
        )?;
        let weth_gnosis = Address::from_str(
            &std::env::var("WETH_GNOSIS")
                .expect("WETH_GNOSIS must be set")
        )?;
        Ok(Self {
            arbitrum_rpc,
            ethereum_rpc,
            gnosis_rpc,
            private_key,
            inbox_arb_to_eth,
            outbox_arb_to_eth,
            inbox_arb_to_gnosis,
            outbox_arb_to_gnosis,
            weth_gnosis,
        })
    }
}
pub struct Providers<P1, P2> {
    pub destination_provider: Arc<P1>,
    pub arbitrum_provider: Arc<P1>,
    pub destination_with_wallet: Arc<P2>,
    pub arbitrum_with_wallet: Arc<P2>,
}
pub fn setup_providers(
    destination_rpc: String,
    arbitrum_rpc: String,
    wallet: EthereumWallet,
) -> Result<
    Providers<impl Provider + Clone + use<>, impl Provider + Clone + use<>>,
    Box<dyn std::error::Error + Send + Sync>
> {
    let destination_provider = ProviderBuilder::new().connect_http(destination_rpc.parse()?);
    let destination_provider = Arc::new(destination_provider);
    let arbitrum_provider = ProviderBuilder::new().connect_http(arbitrum_rpc.parse()?);
    let arbitrum_provider = Arc::new(arbitrum_provider);
    let destination_with_wallet = ProviderBuilder::<_, _, Ethereum>::new()
        .wallet(wallet.clone())
        .connect_provider(destination_provider.clone());
    let destination_with_wallet = Arc::new(destination_with_wallet);
    let arbitrum_with_wallet = ProviderBuilder::<_, _, Ethereum>::new()
        .wallet(wallet)
        .connect_provider(arbitrum_provider.clone());
    let arbitrum_with_wallet = Arc::new(arbitrum_with_wallet);
    Ok(Providers {
        destination_provider,
        arbitrum_provider,
        destination_with_wallet,
        arbitrum_with_wallet,
    })
}
