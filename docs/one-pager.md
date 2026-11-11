# Neap: perps on any token, from any chain, settled on NEAR

**One line.** Open a leveraged long or short on any token that has a DEX pool, on NEAR or Solana,
from the wallet you already have. Every position settles in USDC in one NEAR contract.

## The problem

- **Long-tail tokens have no perps.** A token with a real DEX pool but no Pyth or Chainlink feed and no
  market makers can't be shorted or levered anywhere.
- **Perp venues lock you into one chain.** Using one means bridging, swapping, a new wallet and new gas tokens.
- **Thin markets break the usual designs.** An order book with no makers is empty. A shared vault lets one
  bad memecoin drain every market.

## What Neap does

| | How |
| --- | --- |
| **Any token** | Anyone can create a market for a token by naming its price source: a NEAR DEX pool (read on-chain by the contract) or a pool on another chain (read and signed by a TEE oracle agent). The token itself never moves. Only its price matters. |
| **Any chain** | NEAR users trade directly. Solana users (and later EVM) deposit USDC and open, close or create markets through **NEAR Intents**. Funds are converted to native USDC on NEAR and land in the trading contract. |
| **Pool-to-peer** | Each market has its own USDC liquidity pool (the "seed"), which is the counterparty to every trade. Opposite trades cancel inside the pool, so it only carries the net imbalance. No order book or market makers are needed. |
| **Can't go insolvent** | The risk engine is Percolator (Anatoly Yakovenko's design, ported to NEAR, with formal proofs). If a pool is drained, winners are paid a visible pro-rata share (a "haircut") instead of the venue going bust. Every market is isolated. |
| **Listers get paid** | Whoever creates a market posts a USDC bond and earns **50% of that market's trading fees**. |

## How a trade works

1. **Fund.** Deposit USDC on NEAR, or USDC/SOL from Solana through NEAR Intents (about 10 to 60 seconds, once).
2. **Trade.** Pick a market, leverage and size. The fill lands on NEAR in about 2 to 4 seconds against the market's pool,
   priced from the oracle plus a small spread that grows with the pool's imbalance.
3. **Manage.** The UI shows PnL, liquidation price and funding live. Take-profit and stop-loss orders are run
   by keepers that anyone can operate.
4. **Exit.** Close, then withdraw to NEAR or straight back to Solana.

## What's being built (MVP, hackathon window to ~14 Nov 2026)

| Phase | Scope |
| --- | --- |
| **1: NEAR only** | `neap` contract on NEAR mainnet with tiny caps. Markets for NEAR tokens priced from Rhea/Ref pools, read on-chain. Two-step orders, TP/SL, liquidation, LP pool, lister bonds and fee share. Keeper, indexer, web app. |
| **2: Solana** | TEE oracle agent (NEAR Shade Agent on Phala) reading Solana pools. Markets for Solana tokens. Deposit, open, close and create markets from a Phantom wallet through NEAR Intents. |
| **Later** | Second LP class with a dynamic, utilization-driven yield; more oracle operators; EVM chains; cross-margin; RFQ makers; audit. |

**Demo:** connect Phantom → deposit Solana USDC → open a 5x long on a Solana memecoin → price moves, PnL updates
→ set a stop-loss → close → withdraw back to Solana. Then create a market for a NEAR token from a NEAR wallet.

## Key numbers (MVP defaults)

Trading fee **0.05%** (half to the lister) · liquidation fee **5%** (60% liquidator, 40% insurance) ·
max leverage **10x** on majors, **3 to 5x** on long-tail · fills **~2 to 4 s** · USDC collateral only · isolated margin.

## Why NEAR

One contract holds custody, risk and settlement, so every fill is atomic. NEAR Intents give any-chain deposits
and withdrawals without a bridge UI. Shade Agents (TEEs) bring prices from chains NEAR can't read on-chain.
Access keys make one-click trading possible.

## Team

jason (product, frontend) · Subh (smart contracts). Research and decisions: `moka-trade-labs/moka-research`.
