//! Real-runtime gas measurements and integration flow in the NEAR sandbox.
//!
//! Build the wasm first (from percolator/near):
//!   RUSTFLAGS="--cfg near -C link-arg=-s -C target-cpu=mvp" CARGO_TARGET_DIR=target-wasm \
//!     cargo build --target wasm32-unknown-unknown --release
//!   (and the same for sandbox/mock-ft), or `cargo near build`.
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

#[tokio::test]
async fn sandbox_flow_and_gas() -> anyhow::Result<()> {
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
    // the perp contract must be registered on the token to receive it
    for a in [perp.as_account(), &lp, &bob] {
        ft.call("mint")
            .args_json(json!({"account_id": a.id(), "amount": (if a.id() == perp.id() {"0".to_string()} else {(2_000_000 * USDC).to_string()})}))
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
    call(&owner, perp.id(), "create_market", json!({
        "symbol": "SOL-PERP", "init_price": "100000000", "params": params,
        "spread_bps": 5, "max_oracle_age_ns": "600000000000"
    }), NearToken::from_near(1), &mut g).await?;

    for a in [&lp, &bob] {
        call(a, perp.id(), "register", json!({}), NearToken::from_millinear(100), &mut g).await?;
        let amt = if a.id() == lp.id() { 1_000_000 * USDC } else { 10_000 * USDC };
        let r = a.call(ft.id(), "ft_transfer_call")
            .args_json(json!({"receiver_id": perp.id(), "amount": amt.to_string(), "msg": ""}))
            .deposit(ONE).max_gas().transact().await?;
        anyhow::ensure!(r.is_success(), "ft_transfer_call: {:?}", r.failures());
        g.push(("ft_transfer_call (deposit to wallet)".into(), r.total_gas_burnt));
    }
    call(&lp, perp.id(), "lp_deposit", json!({"market": 0, "amount": (1_000_000 * USDC).to_string()}), NearToken::from_millinear(10), &mut g).await?;
    call(&bob, perp.id(), "open_account", json!({"market": 0}), NearToken::from_millinear(200), &mut g).await?;
    call(&bob, perp.id(), "deposit", json!({"market": 0, "amount": (5_000 * USDC).to_string()}), ONE, &mut g).await?;
    call(&bob, perp.id(), "trade", json!({"market": 0, "size_q": (10 * POS_SCALE).to_string(), "limit_price": "18446744073709551615"}), ONE, &mut g).await?;
    g.last_mut().unwrap().0 = "trade (open 10 units)".into();

    let ts = w.view_block().await?.timestamp();
    call(&oracle, perp.id(), "push_price", json!({"market": 0, "price": "101000000", "publish_ns": ts.to_string()}), NearToken::from_yoctonear(0), &mut g).await?;
    for _ in 0..6 {
        w.fast_forward(2).await?;
        call(&keeper, perp.id(), "crank", json!({"market": 0}), NearToken::from_yoctonear(0), &mut g).await?;
    }
    let m: serde_json::Value = perp.view("get_market").args_json(json!({"market": 0})).await?.json()?;
    anyhow::ensure!(m["effective_price"] == "101000000", "price caught up: {m}");
    call(&keeper, perp.id(), "keeper_step", json!({"market": 0, "account": bob.id()}), NearToken::from_yoctonear(0), &mut g).await?;
    call(&bob, perp.id(), "withdraw_margin", json!({"market": 0, "amount": (1_000 * USDC).to_string()}), ONE, &mut g).await?;
    g.last_mut().unwrap().0 = "withdraw_margin (with open position)".into();
    call(&bob, perp.id(), "trade", json!({"market": 0, "size_q": (-10 * POS_SCALE).to_string(), "limit_price": "0"}), ONE, &mut g).await?;
    g.last_mut().unwrap().0 = "trade (close)".into();
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

    // Liquidation: carol goes ~9x long, price drops 8%, a keeper liquidates.
    let carol = mk("carol").await;
    ft.call("mint").args_json(json!({"account_id": carol.id(), "amount": (1_000 * USDC).to_string()})).transact().await?.into_result()?;
    call(&carol, perp.id(), "register", json!({}), NearToken::from_millinear(100), &mut g).await?;
    let r = carol.call(ft.id(), "ft_transfer_call")
        .args_json(json!({"receiver_id": perp.id(), "amount": (1_000 * USDC).to_string(), "msg": ""}))
        .deposit(ONE).max_gas().transact().await?;
    anyhow::ensure!(r.is_success());
    call(&carol, perp.id(), "open_account", json!({"market": 0}), NearToken::from_millinear(200), &mut g).await?;
    call(&carol, perp.id(), "deposit", json!({"market": 0, "amount": (1_000 * USDC).to_string()}), ONE, &mut g).await?;
    call(&carol, perp.id(), "trade", json!({"market": 0, "size_q": (88 * POS_SCALE).to_string(), "limit_price": "18446744073709551615"}), ONE, &mut g).await?;
    g.last_mut().unwrap().0 = "trade (open 88 units, ~9x)".into();
    let ts = w.view_block().await?.timestamp();
    call(&oracle, perp.id(), "push_price", json!({"market": 0, "price": "93000000", "publish_ns": ts.to_string()}), NearToken::from_yoctonear(0), &mut g).await?;
    for _ in 0..40 {
        w.fast_forward(2).await?;
        call(&keeper, perp.id(), "crank", json!({"market": 0}), NearToken::from_yoctonear(0), &mut g).await?;
        let m: serde_json::Value = perp.view("get_market").args_json(json!({"market": 0})).await?.json()?;
        if m["effective_price"] == "93000000" { break; }
    }
    call(&keeper, perp.id(), "liquidate", json!({"market": 0, "account": carol.id()}), NearToken::from_yoctonear(0), &mut g).await?;
    let a: serde_json::Value = perp.view("get_account").args_json(json!({"market": 0, "account": carol.id()})).await?.json()?;
    println!("carol after liquidation: {a}");
    let (wl, vl): (serde_json::Value, serde_json::Value) = perp.view("get_custody").await?.json()?;
    let m: serde_json::Value = perp.view("get_market").args_json(json!({"market": 0})).await?.json()?;
    anyhow::ensure!(vl == m["vault"], "custody vaults {vl} == engine vault {}", m["vault"]);
    println!("custody: wallets={wl} vaults={vl}; market={m}");

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
