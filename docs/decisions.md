# Decisions

Founder decisions (jason) as of 2026-11-10. Full reasoning and committee debates are in `moka-trade-labs/moka-research`
(`architecture.md`, `docs/decisions/ADR-*`, `docs/committee/`). To reverse a decision, add a dated entry here; never edit silently.

| # | Topic | Decision |
| --- | --- | --- |
| D1 | Execution | Pool-to-peer: a per-market USDC seed pool is the counterparty to every fill. No CLOB, AMM curve, vAMM or RFQ. |
| D2 | Margin | Isolated per market, USDC only. Cross margin later. |
| D3 | Liquidation | Hybrid: permissionless `liquidate` with a reward; the team runs a fallback keeper. |
| D4 | NEAR Intents | Carry cross-chain deposit, open, close and market creation. Everything else (risk, settlement, custody) runs in one NEAR contract. |
| D5 | Phasing | NEAR first: phase 1 is NEAR tokens priced from NEAR pools, all on NEAR. Phase 2 adds Solana prices (TEE) and Solana users (intents). |
| D6 | Price source | An input to `create_market`: `NearPool` (read on-chain by the contract) or `Tee{chain}` (signed by a Shade Agent). Same flow for both. |
| D7 | Assets | No token is bridged or held except USDC. Only the price comes from the outside chain. |
| D8 | Collateral | Native USDC on NEAR. Cross-chain funds are converted by 1Click; NEAR tokens are swapped client-side via Rhea. |
| D9 | Listing | Anyone can list if on-chain rules pass, with a USDC bond. Demo markets are protocol-listed and protocol-seeded. |
| D10 | Lister reward | 50% of the market's trading fees. |
| D11 | Trading fee | 0.05% (5 bps) of notional. Split: lister 50 / seed LP 30 / insurance 10 / protocol 10. |
| D12 | Liquidation fee | 5% of closed notional (capped). Split: liquidator 60 / insurance 40. |
| D13 | Fill model | Two-step request/execute, ~2–4 s, no trusted sequencer. |
| D14 | Unconverted profit (F-1) | The keeper and frontend call `convert_pnl` after every close. |
| D15 | Admin | Hackathon: one limited key (tighten-only, no fund access). Then a 2-of-3 multisig. |
| D16 | Network | NEAR mainnet with tiny caps; demo also from Solana USDC. |
| D17 | Launch markets | SOL, one deep Solana memecoin, one NEAR token. 10x on majors, 3–5x long-tail. |
| D18 | Data | Free tiers only: FastNEAR, Helius free, GeckoTerminal/DexScreener, free Postgres. |
| D19 | Engine | Start from the NEAR-ported Percolator in `moka-research/percolator`. Engine math is not rewritten. |
| D20 | Names | Product Neap, repo `neap`. Contract dev Subh, frontend jason. |
| D21 | Language (2026-11-11) | All off-chain services (keeper, indexer, relayer, oracle agent) are Rust, no TypeScript. Only the frontend (jason) is not Rust. |
