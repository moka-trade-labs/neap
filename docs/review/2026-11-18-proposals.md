# Proposed decisions from the 2026-11-18 design review

Status of every item: **Proposed**. Nothing here is decided. Each one needs jason's answer, and an accepted one
becomes a dated entry in `docs/decisions.md` plus a spec diff in `docs/contract.md` before any code.

Evidence and the debate behind each proposal: [`2026-11-18-design-review.md`](2026-11-18-design-review.md).
All numbers are starting points to simulate, not final values.

| # | Proposal | Touches | Recommendation |
| --- | --- | --- | --- |
| P1 | Solana tokens first, priced by a signed multi-pool median | D5, D17, D6 | Accept |
| P2 | Mark price: time-weighted, worse-of fills for the taker | D6, D13 (refines) | Accept |
| P3 | OI caps computed on-chain from depth and pool NAV | spec §5 (implements) | Accept |
| P4 | Exits and payouts are unconditional | D14 (replaces), spec §2 | Accept |
| P5 | Fee schedule by tier, locked first-loss bond, Unverified and Verified tiers | D9, D10, D11 | Accept, numbers open |
| P6 | Liquidation terms and funding | D12, D17 | Accept for majors; long tail open |
| P7 | Session keys for orders | house rule on 1 yocto | Accept |

---

## P1. Solana tokens first, priced by a signed multi-pool median

**Now.** D5: NEAR tokens from NEAR pools first, Solana in phase 2 through the TEE. D17: launch with SOL at 10x,
one Solana memecoin, one NEAR token.

**Problem.** SOL at 10x costs several times what incumbents charge and nobody will move for it. NEAR tokens deep
enough to list are few. A single on-chain pool read is the weakest price source in the design, and it caps every
market at a small fraction of one pool. The users who would pay to list are Solana token communities.

**Proposal.**
1. Make Solana token markets the first public product. Keep one NEAR-pool market as the on-chain reference path.
2. Price them from a median over at least three pools or sources, time-weighted, signed by at least two
   independent signers, with band and caps enforced on-chain. The Shade Agent becomes one signer, then the only
   kind of signer, as it matures. The trust model is stated plainly in the docs at each step.
3. While any signer is a plain key, listing on that source is curated, not permissionless, and caps stay low.
4. Drop SOL as a "major" at launch, or keep it only as a low-cap calibration market.

**Trade-off.** The on-chain, no-trust price read stops being the headline, and the TEE story arrives in steps
rather than at once. In exchange the first users exist and markets can be bigger than a toy.

**Question for jason.** Is the hackathon pitch tied to the TEE being live at demo time? If yes, keep the demo on
the TEE path and adopt this order for the public launch.

## P2. Mark price

**Now.** `NearPool` mark = EMA of one pool's spot, 5% per sample, unlimited samples per block. Fills at the
engine price walking toward that EMA.

**Problem.** The mark is a forecast anyone can trade against, and anyone can steer how fast it moves.

**Proposal.**
1. Time-weighted mark with at most one sample per block; window fixed per tier.
2. On single-pool sources, the taker fills at the worse of fresh spot and the mark, for opens and closes.
   Margin checks and liquidations use the mark only.
3. Add a price impact term of at least `size / (2 × reference quote reserve)`, so the perp is never cheaper to
   trade than the pool it is priced from.
4. Once a market is priced by the P1 median, closes may fill at the fresh price.

**Trade-off.** Honest traders pay a wider effective spread in fast markets, and a close during a spike fills at
the less favourable price. The trader seat objected to this for closes; the lister and risk seats showed that
closing at one thin pool's spot is exploitable.

## P3. OI caps computed on-chain

**Now.** Caps are 0 (uncapped) until the owner sets them. The spec formula uses 2% of pool depth.

**Proposal.** Recompute on every observation and LP change, with X = the reference quote reserve (24 h minimum):

| Cap | Single-pool source | Signed median (P1) |
| --- | --- | --- |
| Gross per side | 5% of X | 10% of X |
| Net (long minus short) | min(0.5 × pool NAV long tail, 2 × pool NAV major, 3% of X) | same, 5% of X |
| Per account | 1% of X | 2% of X |
| Unverified tier hard ceiling | 25k USD gross | 25k USD gross |

Admin can only lower these. Fills that exceed the room are partially filled up to it.

**Trade-off.** A 100k pool supports about 5k per side. That is small, and it is the honest size of a market
priced from one pool. Bigger markets come from better price sources (P1), not looser caps. The competitor and
trader seats wanted 10% on single pools; the risk seat held at 5%.

## P4. Exits and payouts are unconditional

**Now.** A band breach is a one-way door that stops recording prices. All trades are refused while the engine
price lags. Profit must be converted by the owner within a short window (D14 relies on a keeper call that does
not exist). LP redemption is instant.

**Proposal.**
1. A band breach enters a timed uncertain state, keeps recording observations, and returns to Active after N
   in-band samples and a cooldown. A sustained move re-anchors the mark.
2. Reduce-only fills are allowed during lag and during the uncertain state, at the worse of the engine price and
   the target for the taker.
3. Profit converts inside the closing fill, or any keeper may convert matured, backed profit for an account.
   If neither is safe in the engine, expired backing degrades to a haircut rather than a lock
   (option (c) in `crates/SECURITY.md`). This touches engine kernels and needs its own spec diff and proof.
4. LP redemption is request, then redeem after 24 h at execution-time NAV, as the spec already says.
5. A wind-down path: after a notice period a market settles at the time-weighted price and closes.

**Trade-off.** Slower reaction to a real exploit in (1) and (2); engine work in (3).

## P5. Fees, bond and listing tiers

**Now.** 5 bps everywhere, lister 50 / LP 30 / insurance 10 / protocol 10. The LP share is not paid. The bond is
credited to the lister's wallet and is withdrawable. One permissionless tier with lister-chosen parameters.

**Proposal.**

| | Majors (protocol-listed) | Long tail |
| --- | --- | --- |
| Trading fee | 4 bps, minimum spread near zero | 20 bps, replacing the 20 bps minimum spread |
| Lister | none | 25%, rising toward 50% in proportion to locked first-loss seed |
| LP | 60% | 50% |
| Insurance | 15% | 10% |
| Protocol | 25% | 15% |

- The LP share is credited to pool NAV on every fill.
- The lister earns 5 bps of volume at the base rate, double today's 2.5.

| | Unverified | Verified |
| --- | --- | --- |
| Listing time | about a minute | after graduation |
| Price source | allowlisted DEX only | same, or signed median |
| Reference pool | ≥ 25k quote reserve, ≥ 24 h old | ≥ 50k, ≥ 7 days |
| Bond | 500 USDC, locked as first-loss LP until the market closes, plus a storage fee | 5,000 USDC, same terms |
| Leverage | 2x | 4x |
| Caps | P3, with a 25k hard ceiling | P3 |
| Parameters | protocol-set | protocol-set |
| Shown in the app | by link, badge on the ticket | in the default list |

Graduation: 30 days, a floor of unique traders, no band trips, then a multisig flag.

**Trade-off.** A higher headline fee on long tail and more friction to list. Reverses D10 (50% unconditional)
and the D11 split, and narrows D9. Seats differed by a few points on the split (lister 25 to 30, LP 40 to 50).

**Question for jason.** Is "the lister earns half" a promise already made publicly? If so, keep 50% as the
ceiling a lister reaches by locking seed.

## P6. Liquidation terms and funding

**Now.** Majors IM 10% / MM 8% / liquidation fee 5%. Long tail IM 25% / MM 15% / fee 5%. Funding = cap × pool
net / OI, with the cap chosen by the lister.

**Proposal.**
1. Majors: MM 5%, liquidation fee 1% with a small absolute floor. Envelope: 2.0% + 1.02% + funding = about 3%
   against 5%. Condition: the liquidation sweep runs every 3 s or less, from an on-chain account enumeration
   view, with a second keeper.
2. Long tail: keep MM 15%, cut the fee to 2 to 2.5%. Revisit MM 10% after P2 ships.
3. No liquidation fee is ever charged to the pool account.
4. Skew-reducing fills pay only the minimum spread, as the spec says.
5. Funding becomes a function of net open interest over the net cap (utilisation), convex, protocol-set per tier,
   shown as an hourly rate.

**Trade-off.** Less income per liquidation for liquidators and insurance, and less LP income from idle skew.
Reverses the D12 level (the 60/40 split can stay).

## P7. Session keys for orders

**Now.** Every order needs a full-access-key signature because of the 1 yocto rule. A round trip is five to
seven wallet prompts.

**Proposal.** `authorize_agent(public_key, expiry, notional_limit)`: a key that may request, cancel and place
conditional orders for the account, and nothing else. Withdrawals, LP actions and listing keep the 1 yocto rule.
This is also the shape Solana users need in phase 2 (`execute_signed`).

**Trade-off.** A stolen session key can open bad trades up to the limit until expiry. It cannot move funds out.
This changes the AGENTS.md house rule "every value-moving user call requires 1 yoctoNEAR" for order methods.

---

## Not proposed

- **A shared vault or cross-market counterparty.** All five seats rejected it. An opt-in allocator that deposits
  into isolated pools under per-market limits can come after launch.
- **An order book or RFQ.** D1 stands.
- **Cross margin.** D2 stands for launch.
