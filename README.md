# VEA Validator

Rust validator for the [VEA](https://vea.ninja) cross-chain messaging protocol. Monitors claims, challenges fraud, advances verification, relays L2→L1 messages, and withdraws deposits.

Supports routes:
- Arbitrum → Ethereum
- Arbitrum → Gnosis

## Running with Docker

WIP

## Development

**Prerequisites:**
- [Rust](https://rustup.rs/)
- [Foundry](https://getfoundry.sh/)

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
curl -L https://foundry.paradigm.xyz | bash && foundryup
```

**Fetch VEA contracts:**
```bash
./scripts/fetch-vea-contracts.sh
```

### Local Devnet

Start the local devnet (L1 + L2 anvil chains with deployed contracts):

```bash
./scripts/full-devnet.sh
```

Leave it running. This will automatically create a `.env.local` for you to run the tests.

Run tests on a different terminal:

```bash
source .env.local && cargo test
```

### Testnet

**Funding requirements:**
- Arbitrum Sepolia: ETH for gas
- Sepolia: ETH for gas + deposits
- Chiado: xDAI for gas + [WXDAI](https://gnosis-chiado.blockscout.com/token/0x8d74e5e4DA11629537C4575cB0f33b4F0Dfa42EB) for deposits

```bash
cp .env.example .env.test
```

Edit `.env.test` and add your private key, then:

```bash
source .env.test && cargo run
```

## Configuration

### MAKE_CLAIMS

```bash
export MAKE_CLAIMS=false  # (default) Only monitor and challenge fraud
export MAKE_CLAIMS=true   # Also claim epochs (locks deposit until verified)
```

### RPC Redundancy

RPC URLs support comma-separated values for failover:

```bash
export ETHEREUM_RPC_URL=https://rpc1.example.com,https://rpc2.example.com,https://rpc3.example.com
```

The validator will automatically try the next RPC if one fails.

## Learn More

See [DESIGN_AND_RATIONALE.md](DESIGN_AND_RATIONALE.md) for architecture details.
