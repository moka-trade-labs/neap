#!/bin/sh
# Runs the keeper (if a signing key is set) in a restart loop, and the
# indexer + API in the foreground on $PORT.
set -e
export API_PORT="${API_PORT:-${PORT:-10000}}"
export NEAP_INDEXER_URL="${NEAP_INDEXER_URL:-http://127.0.0.1:${API_PORT}}"

if [ -n "$NEAP_SIGNER_KEY" ]; then
  export FAUCET_ADDR="${FAUCET_ADDR:-127.0.0.1:4001}"
  export KEEPER_INTERNAL_URL="${KEEPER_INTERNAL_URL:-http://${FAUCET_ADDR}}"
  (
    while true; do
      neap-keeper || echo "keeper exited ($?), restarting in 5s"
      sleep 5
    done
  ) &
else
  echo "NEAP_SIGNER_KEY not set: keeper and faucet disabled"
fi

exec neap-indexer
