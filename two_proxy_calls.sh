#!/bin/bash
set -e

NODE="--node https://rpc.uni.juno.deuslabs.fi:443"
TXFLAG="--node https://rpc.uni.juno.deuslabs.fi:443 --chain-id uni-3 --gas-prices 0.025ujunox --gas auto --gas-adjustment 1.3 --broadcast-mode block"

REGISTER_AGENT='{"register_agent":{}}'
PROXY_CALL='{"proxy_call":{}}'

CODE_ID=1061
echo $CODE_ID
INIT='{"denom":"ujunox"}'
GET_TASKS='{"get_tasks":{}}'

junod tx wasm instantiate $CODE_ID "$INIT" --from owner --label "croncat" $TXFLAG -y --no-admin
CONTRACT=$(junod query wasm list-contract-by-code $CODE_ID $NODE --output json | jq -r '.contracts[-1]')

STAKE='{"create_task":{"task":{"interval":"Immediate","boundary":{},"stop_on_fail":false,"actions":[{"msg":{"staking":{"delegate":{"validator":"juno14vhcdsyf83ngsrrqc92kmw8q9xakqjm0ff2dpn","amount":{"denom":"ujunox","amount":"10000"}}}},"gas_limit":150000}],"rules":null}}}'
STAKE2='{"create_task":{"task":{"interval":"Immediate","boundary":{},"stop_on_fail":false,"actions":[{"msg":{"staking":{"delegate":{"validator":"juno14vhcdsyf83ngsrrqc92kmw8q9xakqjm0ff2dpn","amount":{"denom":"ujunox","amount":"20000"}}}},"gas_limit":150000}],"rules":null}}}'
#STAKE3='{"create_task":{"task":{"interval":"Immediate","boundary":{},"stop_on_fail":false,"actions":[{"msg":{"staking":{"delegate":{"validator":"juno14vhcdsyf83ngsrrqc92kmw8q9xakqjm0ff2dpn","amount":{"denom":"ujunox","amount":"30000"}}}},"gas_limit":150000}],"rules":null}}}'

junod tx wasm execute $CONTRACT "$REGISTER_AGENT" --from wallet6 $TXFLAG -y

junod tx wasm execute $CONTRACT "$STAKE" --amount 100000ujunox --from wallet7 $TXFLAG -y
junod tx wasm execute $CONTRACT "$STAKE2" --amount 100000ujunox --from wallet7 $TXFLAG -y
junod query wasm contract-state smart $CONTRACT "$GET_TASKS" $NODE --output json
sleep 10
junod tx wasm execute $CONTRACT "$PROXY_CALL" --from wallet6 $TXFLAG -y
junod query wasm contract-state smart $CONTRACT "$GET_TASKS" $NODE --output json
sleep 10
junod tx wasm execute $CONTRACT "$PROXY_CALL" --from wallet6 $TXFLAG -y
#junod tx wasm execute $CONTRACT "$PROXY_CALL" --from wallet5 $TXFLAG -y
