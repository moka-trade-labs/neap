//! Real-runtime gas measurements and integration flow in the NEAR sandbox.
//!
//! Build the wasm first:
//!   RUSTFLAGS="--cfg near" CARGO_TARGET_DIR=target-wasm \
//!     cargo build --target wasm32-unknown-unknown --release -p neap
//!   cd sandbox/mock-ft && RUSTFLAGS="--cfg near" CARGO_TARGET_DIR=../../target-wasm \
//!     cargo build --target wasm32-unknown-unknown --release
//!   cd sandbox/mock-ref-pool && RUSTFLAGS="--cfg near" CARGO_TARGET_DIR=../../target-wasm \
//!     cargo build --target wasm32-unknown-unknown --release
//! Then: cargo test --release -- --nocapture

use near_workspaces::types::{Gas, NearToken};
use near_workspaces::{Account, Contract};
use serde_json::json;

const USDC: u128 = 1_000_000;
const POS_SCALE: i128 = 1_000_000;
const ONE: NearToken = NearToken::from_yoctonear(1);

fn wasm(name: &str) -> Vec<u8> {
    let p = format!(
        "{}/../target-wasm/wasm32-unknown-unknown/release/{name}.wasm",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&p).unwrap_or_else(|_| panic!("missing {p}; build the wasm first"))
}

async fn call(
    who: &Account,
    c: &near_workspaces::AccountId,
    m: &str,
    args: serde_json::Value,
    dep: NearToken,
    report: &mut Vec<(String, Gas)>,
) -> anyhow::Result<near_workspaces::result::ExecutionFinalResult> {
    let r = who
        .call(c, m)
        .args_json(args)
        .deposit(dep)
        .max_gas()
        .transact()
        .await?;
    if !r.is_success() {
        anyhow::bail!("{m} failed: {:?}", r.failures());
    }
    report.push((m.to_string(), r.total_gas_burnt));
    Ok(r)
}

/// Deploy neap + mock-ft, create a Pushed market, seed the LP pool.
/// Returns (workspace, contract, ft, owner, oracle, lp, bob, keeper, gas_report).
async fn setup_base() -> anyhow::Result<(
    near_workspaces::Worker<near_workspaces::network::Sandbox>,
    Contract,
    Contract,
    Account,
    Account,
    Account,
    Account,
    Account,
    Vec<(String, Gas)>,
)> {
    let w = near_workspaces::sandbox().await?;
    let root = w.root_account()?;
    let mk = |n: &'static str| {
        let root = root.clone();
        async move {
            root.create_subaccount(n)
                .initial_balance(NearToken::from_near(50))
                .transact()
                .await
                .unwrap()
                .into_result()
                .unwrap()
        }
    };
    let owner = mk("owner").await;
    let oracle = mk("oracle").await;
    let lp = mk("lp").await;
    let bob = mk("bob").await;
    let keeper = mk("keeper").await;

    let ft: Contract = w.dev_deploy(&wasm("mock_ft")).await?;
    ft.call("new").transact().await?.into_result()?;
    let perp: Contract = w.dev_deploy(&wasm("neap")).await?;
    let mut g = Vec::new();
    perp.call("new")
        .args_json(json!({"owner": owner.id(), "quote_token": ft.id(), "oracle": oracle.id()}))
        .transact()
        .await?
        .into_result()?;
    for a in [perp.as_account(), &lp, &bob] {
        ft.call("mint")
            .args_json(json!({
                "account_id": a.id(),
                "amount": if a.id() == perp.id() { "0".to_string() } else { (2_000_000 * USDC).to_string() }
            }))
            .transact()
            .await?
            .into_result()?;
    }

    let params = json!({
        "initial_margin_bps": "1000", "maintenance_margin_bps": "500",
        "trading_fee_bps": "10", "liquidation_fee_bps": "50",
        "liquidation_fee_cap": (1_000 * USDC).to_string(), "min_liquidation_abs": "0",
        "max_price_move_bps_per_slot": "30", "max_accrual_dt_slots": "10",
        "max_abs_funding_e9_per_slot": "100", "h_min": "5", "h_max": "50",
        "min_nonzero_mm_req": "1000", "min_nonzero_im_req": "2000"
    });
    call(
        &owner,
        perp.id(),
        "create_market",
        json!({
            "symbol": "SOL-PERP", "init_price": "100000000", "params": params,
            "spread_bps": 5, "max_oracle_age_ns": "600000000000"
        }),
        NearToken::from_near(1),
        &mut g,
    )
    .await?;

    for a in [&lp, &bob] {
        call(a, perp.id(), "register", json!({}), NearToken::from_millinear(100), &mut g).await?;
        let amt = if a.id() == lp.id() { 1_000_000 * USDC } else { 10_000 * USDC };
        let r = a
            .call(ft.id(), "ft_transfer_call")
            .args_json(json!({"receiver_id": perp.id(), "amount": amt.to_string(), "msg": ""}))
            .deposit(ONE)
            .max_gas()
            .transact()
            .await?;
        anyhow::ensure!(r.is_success(), "ft_transfer_call: {:?}", r.failures());
        g.push(("ft_transfer_call (deposit)".into(), r.total_gas_burnt));
    }
    call(
        &lp,
        perp.id(),
        "lp_deposit",
        json!({"market": 0, "amount": (1_000_000 * USDC).to_string()}),
        NearToken::from_millinear(10),
        &mut g,
    )
    .await?;
    call(
        &bob,
        perp.id(),
        "open_account",
        json!({"market": 0}),
        NearToken::from_millinear(200),
        &mut g,
    )
    .await?;
    call(
        &bob,
        perp.id(),
        "deposit",
        json!({"market": 0, "amount": (5_000 * USDC).to_string()}),
        ONE,
        &mut g,
    )
    .await?;

    Ok((w, perp, ft, owner, oracle, lp, bob, keeper, g))
}

#[tokio::test]
async fn sandbox_flow_and_gas() -> anyhow::Result<()> {
    let (w, perp, ft, owner, oracle, lp, bob, keeper, mut g) = setup_base().await?;

    // ── Two-step order: open 10 units ──
    call(
        &bob,
        perp.id(),
        "request_order",
        json!({
            "market": 0, "side": "Long",
            "size_q": (10 * POS_SCALE).to_string(),
            "limit_price": "18446744073709551615",
            "expiry_blocks": 30, "reduce_only": false
        }),
        ONE,
        &mut g,
    )
    .await?;
    g.last_mut().unwrap().0 = "request_order (open 10)".into();

    w.fast_forward(2).await?;
    let ts = w.view_block().await.unwrap().timestamp();
    call(
        &oracle,
        perp.id(),
        "push_price",
        json!({"market": 0, "price": "100000000", "publish_ns": ts.to_string()}),
        NearToken::from_yoctonear(0),
        &mut g,
    )
    .await?;
    call(
        &keeper,
        perp.id(),
        "execute_order",
        json!({"order_id": 0}),
        NearToken::from_yoctonear(0),
        &mut g,
    )
    .await?;
    g.last_mut().unwrap().0 = "execute_order (open 10)".into();

    // ── Oracle moves +1%, crank up ──
    let ts = w.view_block().await.unwrap().timestamp();
    call(
        &oracle,
        perp.id(),
        "push_price",
        json!({"market": 0, "price": "101000000", "publish_ns": ts.to_string()}),
        NearToken::from_yoctonear(0),
        &mut g,
    )
    .await?;
    for _ in 0..6 {
        w.fast_forward(2).await?;
        call(&keeper, perp.id(), "crank", json!({"market": 0}), NearToken::from_yoctonear(0), &mut g).await?;
    }
    let m: serde_json::Value = perp.view("get_market").args_json(json!({"market": 0})).await?.json()?;
    anyhow::ensure!(m["effective_price"] == "101000000", "price caught up: {m}");

    // ── Close via two-step order ──
    call(
        &bob,
        perp.id(),
        "request_order",
        json!({
            "market": 0, "side": "Short",
            "size_q": (10 * POS_SCALE).to_string(),
            "limit_price": "0",
            "expiry_blocks": 30, "reduce_only": true
        }),
        ONE,
        &mut g,
    )
    .await?;
    g.last_mut().unwrap().0 = "request_order (close)".into();

    w.fast_forward(2).await?;
    let ts = w.view_block().await.unwrap().timestamp();
    call(
        &oracle,
        perp.id(),
        "push_price",
        json!({"market": 0, "price": "101000000", "publish_ns": ts.to_string()}),
        NearToken::from_yoctonear(0),
        &mut g,
    )
    .await?;
    call(
        &keeper,
        perp.id(),
        "execute_order",
        json!({"order_id": 1}),
        NearToken::from_yoctonear(0),
        &mut g,
    )
    .await?;
    g.last_mut().unwrap().0 = "execute_order (close)".into();

    // ── Convert PnL and withdraw ──
    w.fast_forward(10).await?;
    call(&keeper, perp.id(), "crank", json!({"market": 0}), NearToken::from_yoctonear(0), &mut g).await?;
    call(&bob, perp.id(), "convert_pnl", json!({"market": 0}), ONE, &mut g).await?;
    let a: serde_json::Value = perp.view("get_account").args_json(json!({"market": 0, "account": bob.id()})).await?.json()?;
    let cap: u128 = a["capital"].as_str().unwrap().parse()?;
    call(&bob, perp.id(), "withdraw_margin", json!({"market": 0, "amount": cap.to_string()}), ONE, &mut g).await?;
    let wal: serde_json::Value = perp.view("get_wallet").args_json(json!({"account": bob.id()})).await?.json()?;
    call(&bob, perp.id(), "withdraw", json!({"amount": wal}), ONE, &mut g).await?;
    let bal: serde_json::Value = ft.view("ft_balance_of").args_json(json!({"account_id": bob.id()})).await?.json()?;
    let bal: u128 = bal.as_str().unwrap().parse()?;
    println!("bob token balance after round trip: {bal} (started 2_000_000 USDC)");
    anyhow::ensure!(bal > 2_000_000 * USDC, "bob realized the +1% move");

    // ── Custody invariant ──
    let (wl, vl): (serde_json::Value, serde_json::Value) = perp.view("get_custody").await?.json()?;
    let m: serde_json::Value = perp.view("get_market").args_json(json!({"market": 0})).await?.json()?;
    anyhow::ensure!(vl == m["vault"], "custody vaults {vl} == engine vault {}", m["vault"]);

    // ── Gas report ──
    println!("\n| method | gas (TGas) |\n|---|---|");
    let mut seen = std::collections::BTreeMap::new();
    for (m, gas) in &g {
        let e = seen.entry(m.clone()).or_insert((0u64, 0u32));
        e.0 = e.0.max(gas.as_gas());
        e.1 += 1;
    }
    for (m, (gmax, n)) in seen {
        println!("| {m} | {:.1} (max of {n}) |", gmax as f64 / 1e12);
    }
    Ok(())
}

/// 1.3: refresh_price with a mock Ref Finance pool.
#[tokio::test]
async fn sandbox_refresh_price_near_pool() -> anyhow::Result<()> {
    let w = near_workspaces::sandbox().await?;
    let root = w.root_account()?;
    let mk = |n: &'static str| {
        let root = root.clone();
        async move {
            root.create_subaccount(n)
                .initial_balance(NearToken::from_near(50))
                .transact()
                .await
                .unwrap()
                .into_result()
                .unwrap()
        }
    };
    let owner = mk("owner").await;
    let oracle = mk("oracle").await;
    let keeper = mk("keeper").await;

    let ft: Contract = w.dev_deploy(&wasm("mock_ft")).await?;
    ft.call("new").transact().await?.into_result()?;

    // Deploy the mock Ref pool with 500_000 USDC and 100_000 tokens
    // → price = 500_000 / 100_000 = 5.0 → price_e6 = 5_000_000.
    let mock_pool: Contract = w.dev_deploy(&wasm("mock_ref_pool")).await?;
    mock_pool
        .call("new")
        .args_json(json!({
            "token_account_ids": [ft.id(), "token.near"],
            "amounts": ["500000000000", "100000000000"]  // 500k USDC, 100k tokens (6 decimals each)
        }))
        .transact()
        .await?
        .into_result()?;

    // Deploy neap with a NearPool market pointing at the mock.
    let perp: Contract = w.dev_deploy(&wasm("neap")).await?;
    perp.call("new")
        .args_json(json!({"owner": owner.id(), "quote_token": ft.id(), "oracle": oracle.id()}))
        .transact()
        .await?
        .into_result()?;

    // Create an owner-managed market first (Pushed) so the contract is initialized.
    // Then we'll test refresh_price manually by creating a NearPool market via
    // the owner's create_market with Pushed, then testing the on_price_read path
    // by calling refresh_price on a second market.
    //
    // Actually, for the NearPool test we need to create a market with NearPool source.
    // The owner create_market always uses Pushed. So we use ft_transfer_call with
    // CreateMarket msg.

    // Register the neap contract on the FT so it can receive tokens.
    ft.call("mint")
        .args_json(json!({"account_id": perp.id(), "amount": "0"}))
        .transact()
        .await?
        .into_result()?;

    // Register the lister on neap.
    let lister = mk("lister").await;
    ft.call("mint")
        .args_json(json!({"account_id": lister.id(), "amount": (100_000 * USDC).to_string()}))
        .transact()
        .await?
        .into_result()?;
    lister
        .call(perp.id(), "register")
        .deposit(NearToken::from_millinear(100))
        .transact()
        .await?
        .into_result()?;

    // Create a NearPool market via ft_transfer_call with bond.
    let params = json!({
        "initial_margin_bps": "1000", "maintenance_margin_bps": "500",
        "trading_fee_bps": "10", "liquidation_fee_bps": "50",
        "liquidation_fee_cap": (1_000 * USDC).to_string(), "min_liquidation_abs": "0",
        "max_price_move_bps_per_slot": "30", "max_accrual_dt_slots": "10",
        "max_abs_funding_e9_per_slot": "100", "h_min": "5", "h_max": "50",
        "min_nonzero_mm_req": "1000", "min_nonzero_im_req": "2000"
    });
    let create_msg = json!({
        "action": "CreateMarket",
        "symbol": "TOKEN-PERP",
        "tier": "LongTail",
        "price_source": {"NearPool": {"dex": mock_pool.id(), "pool_id": 0, "quote_pool": null}},
        "spread_bps": 20,
        "max_oracle_age_ns": "600000000000",
        "params": params
    });
    let r = lister
        .call(ft.id(), "ft_transfer_call")
        .args_json(json!({
            "receiver_id": perp.id(),
            "amount": (50 * USDC).to_string(),
            "msg": create_msg.to_string()
        }))
        .deposit(ONE)
        .max_gas()
        .transact()
        .await?;
    anyhow::ensure!(r.is_success(), "CreateMarket failed: {:?}", r.failures());

    // Verify market 0 is Proposed.
    let m: serde_json::Value = perp.view("get_market").args_json(json!({"market": 0})).await?.json()?;
    anyhow::ensure!(m["mode"] == "Proposed", "market should be Proposed, got: {}", m["mode"]);

    // Call refresh_price — this does a cross-contract call to the mock pool.
    let r = keeper
        .call(perp.id(), "refresh_price")
        .args_json(json!({"market": 0}))
        .max_gas()
        .transact()
        .await?;
    // Print all receipts for debugging.
    for receipt in r.receipt_outcomes() {
        if !receipt.logs.is_empty() {
            println!("receipt logs: {:?}", receipt.logs);
        }
        if receipt.is_failure() {
            println!("receipt failure: {:?}", receipt);
        }
    }
    anyhow::ensure!(r.is_success(), "refresh_price failed: {:?}", r.failures());

    // Check the observation was written.
    let m: serde_json::Value = perp.view("get_market").args_json(json!({"market": 0})).await?.json()?;
    let obs = &m["observation"];
    let price: u64 = obs["price_e6"].as_str().unwrap().parse()?;
    let seq: u64 = obs["seq"].as_str().unwrap().parse()?;
    println!("After refresh_price: price_e6={price}, seq={seq}");
    anyhow::ensure!(price == 5_000_000, "expected price_e6=5000000, got {price}");
    anyhow::ensure!(seq == 1, "expected seq=1, got {seq}");

    // Now activate the market (needs fresh obs + sufficient depth).
    let r = keeper
        .call(perp.id(), "activate_market")
        .args_json(json!({"market": 0}))
        .max_gas()
        .transact()
        .await?;
    anyhow::ensure!(r.is_success(), "activate_market failed: {:?}", r.failures());

    let m: serde_json::Value = perp.view("get_market").args_json(json!({"market": 0})).await?.json()?;
    anyhow::ensure!(m["mode"] == "Active", "market should be Active, got: {}", m["mode"]);
    println!("1.3 + 1.4: NearPool market created, priced via refresh_price, and activated.");

    Ok(())
}
