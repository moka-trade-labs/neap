# keeper

Plan step 1.10. One loop per market: `refresh_price`, `execute_order`, conditional triggers, `liquidate`, `convert_pnl_for`, `crank`.
Holds no funds; the contract re-checks every call. Reads pending work from the indexer, falls back to contract views.
Rust only (binary crate). Empty skeleton crate (workspace member). Not started.
