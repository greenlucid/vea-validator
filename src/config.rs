use alloy::primitives::Address;
use alloy::network::{EthereumWallet, Ethereum};
use alloy::providers::{ProviderBuilder, DynProvider};
use std::str::FromStr;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ChainInfo {
    pub name: String,
    pub rpc_url: String,
    pub deposit_token: Option<Address>,
    pub avg_block_millis: u32,
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
    pub router_provider: Option<DynProvider<Ethereum>>,
    pub router_address: Option<Address>,
    pub amb_address: Option<Address>,
}

pub struct ValidatorConfig {
    pub private_key: String,
    pub wallet: EthereumWallet,
    pub chains: HashMap<u64, ChainInfo>,
    pub inbox_arb_to_eth: Address,
    pub outbox_arb_to_eth: Address,
    pub inbox_arb_to_gnosis: Address,
    pub outbox_arb_to_gnosis: Address,
    pub router_arb_to_gnosis: Address,
    pub arb_outbox: Address,
    pub gnosis_amb: Address,
}
impl ValidatorConfig {
    pub fn build_routes(&self) -> Vec<Route> {
        let arb_rpc = &self.chains.get(&42161).expect("Arbitrum").rpc_url;
        let eth_rpc = &self.chains.get(&1).expect("Ethereum").rpc_url;
        let gnosis_rpc = &self.chains.get(&100).expect("Gnosis").rpc_url;

        let arb_provider = DynProvider::new(
            ProviderBuilder::new()
                .wallet(self.wallet.clone())
                .connect_http(arb_rpc.parse().expect("Invalid Arbitrum RPC URL"))
        );
        let eth_provider = DynProvider::new(
            ProviderBuilder::new()
                .wallet(self.wallet.clone())
                .connect_http(eth_rpc.parse().expect("Invalid Ethereum RPC URL"))
        );
        let gnosis_provider = DynProvider::new(
            ProviderBuilder::new()
                .wallet(self.wallet.clone())
                .connect_http(gnosis_rpc.parse().expect("Invalid Gnosis RPC URL"))
        );

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
                router_provider: None,
                router_address: None,
                amb_address: None,
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
                router_provider: Some(eth_provider.clone()),
                router_address: Some(self.router_arb_to_gnosis),
                amb_address: Some(self.gnosis_amb),
            },
        ]
    }

    pub fn from_env() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        dotenv::dotenv().ok();

        let arbitrum_rpc = std::env::var("ARBITRUM_RPC_URL")
            .expect("ARBITRUM_RPC_URL must be set");
        let ethereum_rpc = std::env::var("ETHEREUM_RPC_URL")
            .expect("ETHEREUM_RPC_URL must be set");
        let gnosis_rpc = std::env::var("GNOSIS_RPC_URL")
            .expect("GNOSIS_RPC_URL must be set");
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
            rpc_url: arbitrum_rpc,
            deposit_token: None,
            avg_block_millis: 250,
        });
        chains.insert(1, ChainInfo {
            name: "Ethereum".to_string(),
            rpc_url: ethereum_rpc,
            deposit_token: None,
            avg_block_millis: 12000,
        });
        chains.insert(100, ChainInfo {
            name: "Gnosis".to_string(),
            rpc_url: gnosis_rpc,
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
        let router_arb_to_gnosis = Address::from_str(
            &std::env::var("VEA_ROUTER_ARB_TO_GNOSIS")
                .expect("VEA_ROUTER_ARB_TO_GNOSIS must be set")
        )?;
        let arb_outbox = Address::from_str(
            &std::env::var("ARB_OUTBOX")
                .expect("ARB_OUTBOX must be set")
        )?;
        let gnosis_amb = Address::from_str(
            &std::env::var("GNOSIS_AMB")
                .expect("GNOSIS_AMB must be set")
        )?;

        Ok(Self {
            private_key,
            wallet,
            chains,
            inbox_arb_to_eth,
            outbox_arb_to_eth,
            inbox_arb_to_gnosis,
            outbox_arb_to_gnosis,
            router_arb_to_gnosis,
            arb_outbox,
            gnosis_amb,
        })
    }
}
