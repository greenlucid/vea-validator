#!/bin/bash

# Fetch VEA contracts from GitHub efficiently
set -e

echo "📦 Fetching VEA contracts from GitHub..."

# Create target directory
mkdir -p contracts/src

# Use sparse checkout to get only the contracts/src folder
cd contracts/src

# Initialize git repo temporarily
git init
git remote add origin https://github.com/kleros/vea.git
git config core.sparseCheckout true

# Only checkout contracts/src folder
echo "contracts/src/*" > .git/info/sparse-checkout

# Fetch and checkout
git fetch --depth 1 origin dev
git checkout origin/dev

# Move contents up and clean git
mv contracts/src/* . 2>/dev/null || true
rm -rf .git contracts

echo "✅ VEA contracts fetched to contracts/src/"
echo "📁 Contents:"
ls -la

cd ../..

echo "📝 Copying custom test contracts..."
cp test-contracts/ArbSysMockForValidator.sol contracts/src/test/bridge-mocks/arbitrum/
cp test-contracts/NodeInterfaceMock.sol contracts/src/test/bridge-mocks/arbitrum/
cp test-contracts/OutboxMock.sol contracts/src/test/bridge-mocks/arbitrum/

echo "🎉 Done!"