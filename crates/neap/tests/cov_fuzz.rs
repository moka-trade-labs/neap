//! Randomized (seeded, dependency-free) sequences of public calls with the
//! invariants of `cov_harness.rs` asserted after every accepted call:
//! custody (inv 1), isolation (inv 2), admin (inv 6), fees (inv 8), LP shares.
//! A refused call is modelled as a reverted receipt (state rebuilt by replay).

#[path = "cov_harness.rs"]
mod h;
use h::*;

const P0: u64 = 100_000_000;
const P1: u64 = 5_000_000;
const TRADERS: [usize; 3] = [T1, T2, T3];
const LPS: [usize; 2] = [LP1, LP2];

fn prelude(f: &mut Fuzz) {
    let mut acts = vec![];
    for (who, amt) in [
        (LP1, 2_000_000u128),
        (LP2, 500_000),
        (T1, 20_000),
        (T2, 20_000),
        (T3, 20_000),
    ] {
        acts.push(Act::Register(who));
        acts.push(Act::FtIn {
            who,
            amount: amt * USDC,
            msg: String::new(),
        });
    }
    acts.push(Act::Register(KEEPER));
    for m in 0..2 {
        acts.push(Act::LpDeposit {
            who: LP1,
            m,
            amount: 500_000 * USDC,
        });
        for who in TRADERS {
            acts.push(Act::OpenAccount { who, m });
            acts.push(Act::Deposit {
                who,
                m,
                amount: 3_000 * USDC,
            });
        }
    }
    for a in acts {
        assert!(f.try_step(a.clone()), "prelude act refused: {a:?}");
    }
}

/// Next small sequence of acts, chosen from the current state.
fn generate(w: &World, r: &mut Rng, allow_liquidate: bool) -> Vec<Act> {
    let m = r.below(2) as u32;
    let who = TRADERS[r.below(3) as usize];
    let lp = LPS[r.below(2) as usize];
    let px = w.target(m);
    let pos = w.position(m, who);
    // The engine refuses an opening fill while any positioned account is
    // unrefreshed in the current block.
    let others_refresh = |m: u32, _who: usize| -> Vec<Act> {
        (0..USERS.len())
            .filter(|i| w.position(m, *i) != 0)
            .map(|i| Act::KeeperStep {
                m,
                who: i,
                by: KEEPER,
            })
            .collect()
    };
    let next_order = w.c.get_next_order_id().0;
    let moved = |bps: i64| -> u64 {
        let p = px as i128 + px as i128 * bps as i128 / 10_000;
        p.clamp(10_000, 10_000_000_000) as u64
    };
    match r.below(100) {
        // open / increase, up to ~8x the account's capital
        0..=17 => {
            let cap = w.capital(m, who);
            let max_q = cap * 8 * POS_SCALE / px as u128;
            let size_q = r.range(1, max_q.max(1));
            let long = r.chance(50);
            let mut v = vec![
                Act::Request {
                    who,
                    m,
                    long,
                    size_q,
                    limit: if long { u64::MAX } else { 0 },
                    expiry: 30,
                    reduce_only: false,
                },
                Act::Tick(1),
                Act::Push {
                    m,
                    price: px,
                    age_ns: 0,
                },
            ];
            v.extend(others_refresh(m, who));
            v.push(Act::Execute {
                order: next_order,
                by: KEEPER,
            });
            v
        }
        // reduce / close
        18..=29 if pos != 0 => {
            let abs = pos.unsigned_abs();
            let size_q = if r.chance(50) { abs } else { r.range(1, abs) };
            vec![
                Act::Request {
                    who,
                    m,
                    long: pos < 0,
                    size_q,
                    limit: if pos < 0 { u64::MAX } else { 0 },
                    expiry: 30,
                    reduce_only: true,
                },
                Act::Tick(1),
                Act::Push {
                    m,
                    price: px,
                    age_ns: 0,
                },
                Act::Execute {
                    order: next_order,
                    by: KEEPER,
                },
            ]
        }
        // small move
        30..=37 => {
            let bps = r.below(61) as i64 - 30;
            vec![
                Act::Tick(1),
                Act::Push {
                    m,
                    price: moved(bps),
                    age_ns: 0,
                },
                Act::Walk { m, max: 5 },
            ]
        }
        // big move along the staircase
        38..=43 => {
            let bps = r.below(1_201) as i64 - 600;
            vec![
                Act::Tick(1),
                Act::Push {
                    m,
                    price: moved(bps),
                    age_ns: 0,
                },
                Act::Walk { m, max: 80 },
            ]
        }
        // time passes (funding accrues), keeper cranks
        44..=49 => vec![Act::Tick(1 + r.below(15)), Act::Crank { m }],
        50..=54 => vec![Act::Deposit {
            who,
            m,
            amount: r.range(1, w.wallet(who).max(1)),
        }],
        55..=59 => vec![
            Act::Tick(1),
            Act::Push {
                m,
                price: px,
                age_ns: 0,
            },
            Act::WithdrawMargin {
                who,
                m,
                amount: r.range(1, w.capital(m, who).max(1)),
            },
        ],
        60..=63 => vec![
            Act::Tick(1),
            Act::Push {
                m,
                price: px,
                age_ns: 0,
            },
            Act::Convert { who, m },
        ],
        64..=68 => vec![Act::KeeperStep { m, who, by: KEEPER }],
        69..=72 => vec![
            Act::Tick(1),
            Act::Push {
                m,
                price: px,
                age_ns: 0,
            },
            Act::LpDeposit {
                who: lp,
                m,
                amount: r.range(1, w.wallet(lp).max(1)),
            },
        ],
        73..=76 => {
            let held = w.c.get_lp_shares(m, user(lp)).0;
            vec![Act::LpRedeem {
                who: lp,
                m,
                shares: r.range(1, held.max(1)),
            }]
        }
        77..=79 => vec![Act::Insurance {
            who: lp,
            m,
            amount: r.range(1, 1_000 * USDC),
        }],
        80..=83 => vec![Act::ClaimFees {
            m,
            protocol: r.chance(50),
        }],
        84..=87 => {
            let u = r.below(USERS.len() as u64) as usize;
            vec![Act::Withdraw {
                who: u,
                amount: r.range(1, w.wallet(u).max(1)),
                transfer_ok: r.chance(50),
            }]
        }
        88..=89 => vec![Act::FtIn {
            who,
            amount: r.range(1, 5_000 * USDC),
            msg: String::new(),
        }],
        // conditionals: place near the mark, try to execute a random live one
        90..=92 => {
            let kind = r.below(3) as u8;
            let long = if pos != 0 { pos < 0 } else { r.chance(50) };
            let trigger = moved(r.below(401) as i64 - 200);
            vec![Act::PlaceCond {
                who,
                m,
                kind,
                long,
                size_q: if pos != 0 {
                    pos.unsigned_abs()
                } else {
                    POS_SCALE
                },
                trigger,
                limit: if long { u64::MAX } else { 0 },
                expiry: 1 + r.below(400),
                reduce_only: true,
            }]
        }
        93..=95 if !w.conds.is_empty() => {
            let cond = w.conds[r.below(w.conds.len() as u64) as usize];
            if r.chance(25) {
                let owner = w.c.get_conditional(cond).map(|c| c.owner);
                let who = TRADERS
                    .into_iter()
                    .find(|t| Some(neap::model::Owner::Near(user(*t))) == owner)
                    .unwrap_or(T1);
                vec![Act::CancelCond { cond, who }]
            } else {
                vec![Act::ExecCond { cond, by: KEEPER }]
            }
        }
        // stale / cancelled orders
        96 if !w.orders.is_empty() => {
            let order = w.orders[r.below(w.orders.len() as u64) as usize];
            vec![Act::Execute { order, by: KEEPER }]
        }
        97 => vec![
            Act::Request {
                who,
                m,
                long: true,
                size_q: POS_SCALE,
                limit: u64::MAX,
                expiry: 3,
                reduce_only: false,
            },
            if r.chance(50) {
                Act::Cancel {
                    order: next_order,
                    who,
                }
            } else {
                Act::Tick(5)
            },
        ],
        // admin
        98 => match r.below(4) {
            0 => vec![Act::SetOiCaps {
                m,
                long: r.range(0, 5_000) * POS_SCALE,
                short: r.range(0, 5_000) * POS_SCALE,
                acct: r.range(0, 2_000) * POS_SCALE,
            }],
            1 => vec![Act::SetPauser(Some(KEEPER))],
            2 => vec![Act::SetOracle],
            _ => vec![Act::SetOwner],
        },
        _ => {
            if allow_liquidate {
                let victim = TRADERS[r.below(3) as usize];
                vec![
                    Act::Tick(1),
                    Act::Push {
                        m,
                        price: px,
                        age_ns: 0,
                    },
                    Act::Liquidate {
                        m,
                        who: victim,
                        by: KEEPER,
                    },
                ]
            } else {
                vec![Act::KeeperStep { m, who, by: KEEPER }]
            }
        }
    }
}

struct Stats {
    accepted: u64,
    refused: u64,
    fills: u64,
    liquidations: u64,
    claimed: u128,
    failed_withdraws: u64,
    lp_redeems: u64,
    conds: u64,
}

fn run(seed: u64, rounds: usize, allow_liquidate: bool) -> Stats {
    let mut f = Fuzz::new(P0, P1);
    prelude(&mut f);
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    for _ in 0..rounds {
        let mut acts = generate(f.w(), &mut r, allow_liquidate);
        // Everything except `crank` needs the market caught up to this block.
        if !acts
            .iter()
            .any(|a| matches!(a, Act::Push { .. } | Act::Crank { .. }))
        {
            if let Some(m) = acts[0].market() {
                acts.insert(0, Act::Crank { m });
            }
        }
        for a in acts {
            if !f.try_step(a) {
                break; // reverted: skip the rest of this sequence
            }
        }
    }
    for (k, v) in &f.refusals {
        println!("  seed {seed} refused {v:>4}  {k}");
    }
    let w = f.w();
    Stats {
        accepted: f.accepted,
        refused: f.refused,
        fills: w.fills,
        liquidations: w.liquidations,
        claimed: w.fees_claimed.iter().sum(),
        failed_withdraws: w
            .log
            .iter()
            .filter(|a| {
                matches!(
                    a,
                    Act::Withdraw {
                        transfer_ok: false,
                        ..
                    }
                )
            })
            .count() as u64,
        lp_redeems: w
            .log
            .iter()
            .filter(|a| matches!(a, Act::LpRedeem { .. }))
            .count() as u64,
        conds: w
            .log
            .iter()
            .filter(|a| matches!(a, Act::ExecCond { .. }))
            .count() as u64,
    }
}

fn run_seeds(seeds: std::ops::Range<u64>, rounds: usize, allow_liquidate: bool) -> Stats {
    let mut t = Stats {
        accepted: 0,
        refused: 0,
        fills: 0,
        liquidations: 0,
        claimed: 0,
        failed_withdraws: 0,
        lp_redeems: 0,
        conds: 0,
    };
    for seed in seeds {
        let s = run(seed, rounds, allow_liquidate);
        println!(
            "seed {seed}: accepted {} refused {} fills {} liquidations {} claimed {} failed-withdraws {} lp-redeems {} cond-execs {}",
            s.accepted, s.refused, s.fills, s.liquidations, s.claimed, s.failed_withdraws, s.lp_redeems, s.conds
        );
        t.accepted += s.accepted;
        t.refused += s.refused;
        t.fills += s.fills;
        t.liquidations += s.liquidations;
        t.claimed += s.claimed;
        t.failed_withdraws += s.failed_withdraws;
        t.lp_redeems += s.lp_redeems;
        t.conds += s.conds;
    }
    t
}

/// Short run, part of `cargo test -p neap` (about 10 s). `liquidate` is in the
/// mix but random play rarely makes an account liquidatable; the
/// deterministic liquidation is in `cov_custody_scenario.rs`.
#[test]
fn fuzz_invariants_hold_smoke() {
    let t = run_seeds(1..2, 110, true);
    // Not vacuous: calls were accepted, refused (and rolled back), and filled.
    assert!(t.accepted > 150, "accepted {}", t.accepted);
    assert!(t.refused > 0, "no refused call was rolled back");
    assert!(t.fills > 0, "no fill");
}

/// Long run: `cargo test -p neap --test cov_fuzz -- --ignored` (minutes).
#[test]
#[ignore = "long: run nightly or before a release"]
fn fuzz_invariants_hold_long() {
    let t = run_seeds(1..13, 220, true);
    assert!(t.fills > 100, "fills {}", t.fills);
    assert!(t.claimed > 0, "no fee claim paid out");
    assert!(t.failed_withdraws > 0, "no failed-withdraw callback");
    assert!(t.lp_redeems > 0, "no LP redeem");
    assert!(t.refused > 0, "no refused call was rolled back");
}
