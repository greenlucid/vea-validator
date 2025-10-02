#!/bin/bash

set -e

# Kill any existing anvil processes
pkill -f "anvil.*854[567]" || true

# Start anvil instances
anvil -p 8545 --chain-id 31337 > /tmp/arbitrum-devnet.log 2>&1 &
ARBITRUM_PID=$!

anvil -p 8546 --chain-id 31338 > /tmp/mainnet-devnet.log 2>&1 &
MAINNET_PID=$!

anvil -p 8547 --chain-id 31339 > /tmp/gnosis-devnet.log 2>&1 &
GNOSIS_PID=$!

# Cleanup on exit
trap 'kill $ARBITRUM_PID $MAINNET_PID $GNOSIS_PID 2>/dev/null || true' INT TERM EXIT

echo "Starting devnet..."
sleep 3

# Check if devnets are running
if ! curl -s -H "Content-Type: application/json" \
      --data '{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}' localhost:8545 >/dev/null; then
    echo "Failed to start Arbitrum devnet"
    exit 1
fi

if ! curl -s -H "Content-Type: application/json" \
      --data '{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}' localhost:8546 >/dev/null; then
    echo "Failed to start Mainnet devnet"
    exit 1
fi

if ! curl -s -H "Content-Type: application/json" \
      --data '{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}' localhost:8547 >/dev/null; then
    echo "Failed to start Gnosis devnet"
    exit 1
fi

echo "Devnet running: Arbitrum on 8545 (PID $ARBITRUM_PID), Mainnet on 8546 (PID $MAINNET_PID), Gnosis on 8547 (PID $GNOSIS_PID)"

echo "Deploying contracts..."

ARB_RPC="http://localhost:8545"
ETH_RPC="http://localhost:8546"
GNOSIS_RPC="http://localhost:8547"
PK="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"

forge build -q

FROM="$(cast wallet address "$PK")"
NONCE="$(cast nonce "$FROM" --rpc-url "$ARB_RPC")"
INBOX_PREDICTED="$(cast compute-address "$FROM" --nonce "$NONCE" \
  | awk '/Computed Address:/ {print $3}')"

create() {
  forge create "$1" \
    --rpc-url "$2" \
    --private-key "$PK" \
    --broadcast \
    "${@:3}"
}

addr_of() {
  awk '/Deployed to:/ {print $3}'
}

SEQ_INBOX="$(
  create contracts/src/test/bridge-mocks/arbitrum/SequencerInboxMock.sol:SequencerInboxMock \
    "$ETH_RPC" \
    --constructor-args 86400 | addr_of
)"

OUTBOX_MOCK="$(
  create contracts/src/test/bridge-mocks/arbitrum/OutboxMock.sol:OutboxMock \
    "$ETH_RPC" \
    --constructor-args "$INBOX_PREDICTED" | addr_of
)"

BRIDGE="$(
  create contracts/src/test/bridge-mocks/arbitrum/BridgeMock.sol:BridgeMock \
    "$ETH_RPC" \
    --constructor-args "$OUTBOX_MOCK" "$SEQ_INBOX" | addr_of
)"

OUTBOX="$(
  create contracts/src/arbitrumToEth/VeaOutboxArbToEth.sol:VeaOutboxArbToEth \
    "$ETH_RPC" \
    --constructor-args \
      1000000000000000000 \
      3600 \
      600 \
      24 \
      "$INBOX_PREDICTED" \
      "$BRIDGE" \
      10 | addr_of
)"

INBOX="$(
  create contracts/src/arbitrumToEth/VeaInboxArbToEth.sol:VeaInboxArbToEth \
    "$ARB_RPC" \
    --constructor-args 3600 "$OUTBOX" | addr_of
)"

# Deploy AMB mocks for Gnosis bridge
AMB_MAINNET="$(
  create contracts/src/test/bridge-mocks/gnosis/MockAMB.sol:MockAMB \
    "$ETH_RPC" | addr_of
)"

AMB_GNOSIS="$(
  create contracts/src/test/bridge-mocks/gnosis/MockAMB.sol:MockAMB \
    "$GNOSIS_RPC" | addr_of
)"

# Deploy WETH mock on Gnosis (needed for VeaOutboxArbToGnosis)
WETH_GNOSIS="$(
  create contracts/src/test/tokens/gnosis/MockWETH.sol:MockWETH \
    "$GNOSIS_RPC" | addr_of
)"

# Deploy VeaInbox for Arbitrum to Gnosis
INBOX_ARB_TO_GNOSIS="$(
  create contracts/src/arbitrumToGnosis/VeaInboxArbToGnosis.sol:VeaInboxArbToGnosis \
    "$ARB_RPC" \
    --constructor-args 3600 "$BRIDGE" | addr_of
)"

# Predict VeaOutbox address on Gnosis
NONCE_GNOSIS="$(cast nonce "$FROM" --rpc-url "$GNOSIS_RPC")"
OUTBOX_ARB_TO_GNOSIS_PREDICTED="$(cast compute-address "$FROM" --nonce "$NONCE_GNOSIS" \
  | awk '/Computed Address:/ {print $3}')"

# Deploy Router on Mainnet
ROUTER_ARB_TO_GNOSIS="$(
  create contracts/src/arbitrumToGnosis/RouterArbToGnosis.sol:RouterArbToGnosis \
    "$ETH_RPC" \
    --constructor-args "$BRIDGE" "$AMB_MAINNET" "$INBOX_ARB_TO_GNOSIS" \
      "$OUTBOX_ARB_TO_GNOSIS_PREDICTED" | addr_of
)"

# Deploy VeaOutbox on Gnosis
# Args: deposit, epochPeriod, minChallengePeriod, timeoutEpochs, amb, router, sequencerDelayLimit, maxMissingBlocks, routerChainId, weth
OUTBOX_ARB_TO_GNOSIS="$(
  create contracts/src/devnets/arbitrumToGnosis/VeaOutboxArbToGnosisDevnet.sol:VeaOutboxArbToGnosisDevnet \
    "$GNOSIS_RPC" \
    --constructor-args 1000000000000000000 3600 600 2 \
      "$AMB_GNOSIS" "$ROUTER_ARB_TO_GNOSIS" 86400 10 31338 "$WETH_GNOSIS" | addr_of
)"

# Configure AMBs
cast send "$AMB_MAINNET" "setMaxGasPerTx(uint256)" 2000000 \
  --private-key "$PK" --rpc-url "$ETH_RPC" >/dev/null

cast send "$AMB_GNOSIS" "setMaxGasPerTx(uint256)" 2000000 \
  --private-key "$PK" --rpc-url "$GNOSIS_RPC" >/dev/null

cat > .env.local <<EOF
ARBITRUM_RPC_URL=$ARB_RPC
MAINNET_RPC_URL=$ETH_RPC
ETHEREUM_RPC_URL=$ETH_RPC
GNOSIS_RPC_URL=$GNOSIS_RPC

# Arbitrum to Ethereum contracts
VEA_INBOX_ARB_TO_ETH=$INBOX
VEA_OUTBOX_ARB_TO_ETH=$OUTBOX

# Arbitrum to Gnosis contracts
VEA_INBOX_ARB_TO_GNOSIS=$INBOX_ARB_TO_GNOSIS
VEA_OUTBOX_ARB_TO_GNOSIS=$OUTBOX_ARB_TO_GNOSIS
ROUTER_ARB_TO_GNOSIS=$ROUTER_ARB_TO_GNOSIS

# Bridge mocks
SEQUENCER_INBOX_MOCK=$SEQ_INBOX
OUTBOX_MOCK=$OUTBOX_MOCK
BRIDGE_MOCK=$BRIDGE
AMB_MAINNET=$AMB_MAINNET
AMB_GNOSIS=$AMB_GNOSIS
WETH_GNOSIS=$WETH_GNOSIS
EOF

echo "Arbitrum → Ethereum:"
echo "  VeaInbox (Arbitrum):  $INBOX"
echo "  VeaOutbox (Mainnet):  $OUTBOX"
echo ""
echo "Arbitrum → Gnosis:"
echo "  VeaInbox (Arbitrum):  $INBOX_ARB_TO_GNOSIS"
echo "  Router (Mainnet):     $ROUTER_ARB_TO_GNOSIS"
echo "  VeaOutbox (Gnosis):   $OUTBOX_ARB_TO_GNOSIS"
echo ""
echo "Bridge Mocks:"
echo "  Bridge:               $BRIDGE"
echo "  SequencerInbox:       $SEQ_INBOX"
echo "  OutboxMock:           $OUTBOX_MOCK"
echo "  AMB (Mainnet):        $AMB_MAINNET"
echo "  AMB (Gnosis):         $AMB_GNOSIS"

echo "✅ Full devnet ready!"
echo "🛑 Press Ctrl+C to stop"

# Keep running until interrupted
wait