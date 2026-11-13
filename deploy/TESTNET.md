# Testnet deployment (team-controlled copy, 2026-11-13)

| Account | Role |
|---------|------|
| `neap-perps.testnet` | Contract + owner |
| `neap-oracle.testnet` | Oracle + keeper signer (pushes prices, executes orders, faucet mints) |
| `lp.neap-perps.testnet` | Seed LP (50,000 test USDC in market 0) |
| `demo.neap-perps.testnet` | Smoke-test trader |
| `usdc.fakes.testnet` | Quote token (public `mint`, 6 decimals) |

Keys are **not** in the repo. The oracle key lives only in Render (`NEAP_SIGNER_KEY`).

- Backend: https://neap-backend.onrender.com (`render.yaml`, Render blueprint `neap`)
- Market 0: `NEAR-PERP`, Pushed source, live NEAR/USD from the keeper, same params as the first deployment.
- Init: `new(owner: neap-perps.testnet, quote_token: usdc.fakes.testnet, oracle: neap-oracle.testnet)`,
  deploy tx `GkMbikzkNFnvEFVTngq1YCcVHzZRwkTZUSWwWsmbmUqj`.
- The contract account itself can't hold a wallet ("reserved account"), so LP seeding goes through a sub-account.
- Gas: the keeper spends about 0.4 NEAR/hour at one market. Top up `neap-oracle.testnet` when it runs low.

Ops without near-cli: `cargo run -p neap-ops -- <keygen|view|call|deploy|sub|transfer>` (see `services/ops/src/main.rs`).
The previous deployment (`neap-trade.testnet`) is described in `CONTRACT.md`.

## Path 1 demo: a NEAR token priced from a pool (2026-11-13)

- Demo token `tide-token.testnet` (TIDE, 6 decimals, open `mint`).
- Ref testnet pool: `ref-finance-101.testnet`, `pool_id` 2962, 100,000 TIDE / 250,000 test USDC ($2.50).
- Market 1 `TIDE-PERP` was created through `ft_transfer_call` + `CreateMarket` (LongTail, 50 USDC bond,
  `NearPool{dex, pool_id: 2962}`), activated by the keeper, seeded with 20,000 USDC of LP.
- `CreateMarket` accepts `base_decimals` (default 6) so 8/18/24-decimal tokens price correctly.
  The contract was upgraded in place (tx `ArZS8YRDZEHvqpXcRHdueJ6xu9XuzwrenQ9EykF7R5ux`); the value lives
  under a raw storage key `bd:<market>`, so no state migration was needed. It is lister-supplied and
  not checked against `ft_metadata`.
