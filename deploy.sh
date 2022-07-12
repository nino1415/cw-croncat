#!/bin/bash
set -ex

cargo wasm
# In case of M1 MacBook use rust-optimizer-arm64 instead of rust-optimizer
docker run --rm -v "$(pwd)":/code \
  --mount type=volume,source="$(basename "$(pwd)")_cache",target=/code/target \
  --mount type=volume,source=registry_cache,target=/usr/local/cargo/registry \
  cosmwasm/rust-optimizer-arm64:0.12.6

NODE="--node https://rpc.uni.juno.deuslabs.fi:443"
TXFLAG="--node https://rpc.uni.juno.deuslabs.fi:443 --chain-id uni-3 --gas-prices 0.025ujunox --gas auto --gas-adjustment 1.3 --broadcast-mode block"

# In case of M1 MacBook replace cw_croncat.wasm with cw_croncat-aarch64.wasm 
RES=$(junod tx wasm store artifacts/cw_croncat-aarch64.wasm --from owner $TXFLAG -y --output json -b block)
CODE_ID=$(echo $RES | jq -r '.logs[0].events[-1].attributes[0].value')
echo $CODE_ID

