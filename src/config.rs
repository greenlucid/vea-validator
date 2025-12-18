use alloy::primitives::Address;
use alloy::network::{EthereumWallet, Ethereum};
use alloy::providers::{ProviderBuilder, DynProvider};
use alloy::rpc::client::RpcClient;
use alloy::transports::http::Http;
use alloy::transports::layers::FallbackLayer;
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::collections::HashMap;
use tower::ServiceBuilder;

#[derive(Debug, Clone)]
pub struct ChainInfo {
    pub name: String,
    pub rpc_urls: Vec<String>,
    pub deposit_token: Option<Address>,
    pub avg_block_millis: u32,
}

#[derive(Clone)]
pub struct RouteSettings {
    pub relay_delay_secs: u64,
    pub start_verification_delay: u64,
    pub min_challenge_period: u64,
    pub sync_lookback_secs: u64,
}

impl RouteSettings {
    pub fn test_defaults() -> Self {
        Self {
            relay_delay_secs: 7 * 24 * 3600,
            start_verification_delay: 86400 + 3600,
            min_challenge_period: 600,
            sync_lookback_secs: 7 * 24 * 3600 + 24 * 3600,
        }
    }
}

#[derive(Clone)]
pub struct Route {
    pub name: &'static str,
    pub inbox_chain_id: u64,
    pub inbox_address: Address,
    pub inbox_provider: DynProvider<Ethereum>,
    pub inbox_avg_block_millis: u32,
    pub outbox_chain_id: u64,
    pub outbox_address: Address,
    pub outbox_provider: DynProvider<Ethereum>,
    pub weth_address: Option<Address>,
    pub settings: RouteSettings,
}

#[derive(Clone)]
pub struct ValidatorConfig {
    pub private_key: String,
    pub wallet: EthereumWallet,
    pub chains: HashMap<u64, ChainInfo>,
    pub inbox_arb_to_eth: Address,
    pub outbox_arb_to_eth: Address,
    pub inbox_arb_to_gnosis: Address,
    pub outbox_arb_to_gnosis: Address,
    pub arb_outbox: Address,
    pub make_claims: bool,
}
impl ValidatorConfig {
    fn build_provider(&self, chain_id: u64) -> DynProvider<Ethereum> {
        let chain = self.chains.get(&chain_id).expect("Chain not found");
        let urls = &chain.rpc_urls;

        if urls.len() == 1 {
            return DynProvider::new(
                ProviderBuilder::new()
                    .wallet(self.wallet.clone())
                    .connect_http(urls[0].parse().expect("Invalid RPC URL"))
            );
        }

        let fallback = FallbackLayer::default()
            .with_active_transport_count(NonZeroUsize::new(1).unwrap());

        let transports: Vec<_> = urls.iter()
            .map(|url| Http::new(url.parse().expect("Invalid RPC URL")))
            .collect();

        let transport = ServiceBuilder::new()
            .layer(fallback)
            .service(transports);

        let client = RpcClient::builder().transport(transport, false);
        DynProvider::new(
            ProviderBuilder::new()
                .wallet(self.wallet.clone())
                .connect_client(client)
        )
    }

    pub fn build_routes(&self) -> Vec<Route> {
        let arb_provider = self.build_provider(42161);
        let eth_provider = self.build_provider(1);
        let gnosis_provider = self.build_provider(100);

        vec![
            Route {
                name: "ARB_TO_ETH",
                inbox_chain_id: 42161,
                inbox_address: self.inbox_arb_to_eth,
                inbox_provider: arb_provider.clone(),
                inbox_avg_block_millis: 250,
                outbox_chain_id: 1,
                outbox_address: self.outbox_arb_to_eth,
                outbox_provider: eth_provider.clone(),
                weth_address: self.chains.get(&1).expect("Ethereum").deposit_token,
                settings: RouteSettings::test_defaults(),
            },
            Route {
                name: "ARB_TO_GNOSIS",
                inbox_chain_id: 42161,
                inbox_address: self.inbox_arb_to_gnosis,
                inbox_provider: arb_provider.clone(),
                inbox_avg_block_millis: 250,
                outbox_chain_id: 100,
                outbox_address: self.outbox_arb_to_gnosis,
                outbox_provider: gnosis_provider.clone(),
                weth_address: self.chains.get(&100).expect("Gnosis").deposit_token,
                settings: RouteSettings::test_defaults(),
            },
        ]
    }

    fn parse_rpc_urls(env_var: &str) -> Vec<String> {
        std::env::var(env_var)
            .expect(&format!("{} must be set", env_var))
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    pub fn from_env() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {

        let arbitrum_rpcs = Self::parse_rpc_urls("ARBITRUM_RPC_URL");
        let ethereum_rpcs = Self::parse_rpc_urls("ETHEREUM_RPC_URL");
        let gnosis_rpcs = Self::parse_rpc_urls("GNOSIS_RPC_URL");
        let weth_gnosis = Address::from_str(
            &std::env::var("WETH_GNOSIS")
                .expect("WETH_GNOSIS must be set")
        )?;

        let private_key = std::env::var("PRIVATE_KEY")
            .or_else(|_| std::fs::read_to_string("/run/secrets/validator_key")
                .map(|s| s.trim().to_string()))
            .expect("PRIVATE_KEY not set or /run/secrets/validator_key not found");

        use alloy::signers::local::PrivateKeySigner;
        let signer = PrivateKeySigner::from_str(&private_key)?;
        let wallet = EthereumWallet::from(signer);

        let mut chains = HashMap::new();
        chains.insert(42161, ChainInfo {
            name: "Arbitrum".to_string(),
            rpc_urls: arbitrum_rpcs,
            deposit_token: None,
            avg_block_millis: 250,
        });
        chains.insert(1, ChainInfo {
            name: "Ethereum".to_string(),
            rpc_urls: ethereum_rpcs,
            deposit_token: None,
            avg_block_millis: 12000,
        });
        chains.insert(100, ChainInfo {
            name: "Gnosis".to_string(),
            rpc_urls: gnosis_rpcs,
            deposit_token: Some(weth_gnosis),
            avg_block_millis: 5000,
        });

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
        let arb_outbox = Address::from_str(
            &std::env::var("ARB_OUTBOX")
                .expect("ARB_OUTBOX must be set")
        )?;

        let make_claims = std::env::var("MAKE_CLAIMS")
            .map(|v| v.to_lowercase() == "true" || v == "1")
            .expect("MAKE_CLAIMS must be set");

        Ok(Self {
            private_key,
            wallet,
            chains,
            inbox_arb_to_eth,
            outbox_arb_to_eth,
            inbox_arb_to_gnosis,
            outbox_arb_to_gnosis,
            arb_outbox,
            make_claims,
        })
    }
}
