# Neap design review, 2026-11-18

Scope: the product and mechanism design, not code quality. Question asked by jason: what stands between today's
Neap and a production grade perp venue, in the idea and in the experience it gives traders, LPs and listers.

Nothing in this file changes a decision. Changes to D1–D21 are written as proposals in
[`2026-11-18-proposals.md`](2026-11-18-proposals.md) for jason to accept, change or reject.

## How this was done

Five reviewers read both repos (`neap` at `d153dbe`, `neap-frontend` at `29088c1`) independently, each from one
seat: an active perp trader, a liquidity provider, a market lister who also played attacker, a risk skeptic and a
competitor's head of product. Round one was a ranked list from each seat. In round two each seat read the others'
findings, checked them against the code and answered seven open disputes. The chair then ranked the result.

Limits: nothing was executed or simulated. All attack costs and yields are hand arithmetic on a constant-product
pool. Statements about other venues are from the reviewers' memory unless marked "searched". The numbers proposed
here (caps, fees, margins) are starting points to be simulated, not final values.

## Verdict by seat

| Seat | Would they use it today? | What would change their mind |
| --- | --- | --- |
| Trader | Not for daily flow. Only for tokens with no perp elsewhere, at small size. | Exits that always work, a mark that is not a lagging copy of spot, majors priced near incumbents or dropped. |
| LP | No deposit. | OI caps computed on-chain, the LP fee share actually paid, a mark that cannot be front-run, a redeem delay. Then about 250k USD per major; long tail only behind a lister first-loss tranche. |
| Risk skeptic | Would not LP and would fear trading here. | Caps that scale with the quality of the price source, a mark that costs more to move than the perp pays, liquidation that does not depend on one 15 s keeper loop. |
| Honest lister | Would not list. 2.5 bps on a tiny capped market pays a few dollars a day, and one 20% candle kills the market for good. | A higher long-tail fee, an automatic way back to Active, clear ownership and wind-down. |
| Malicious lister | Would list today. | See finding 1. |
| Competitor | Not worried today. Worried if Neap ships Solana token markets with lister revenue share before the Percolator-based Solana launchers leave beta. | That is the wedge: "list your token's perp in a minute and earn from it". |

## What is right and must not be lost

- **Per-market isolation with a visible pro-rata haircut.** Every attack found below is capped at one market's
  deposits. This is what makes permissionless listing defensible at all. All five seats said keep it (D1, D2).
- **Asymmetric LP pricing.** Deposit at the upper NAV bound, redeem at the lower, virtual shares, deposit refused
  while the price is catching up (`policy.rs:95-147`, `contract.rs:1402`).
- **The lister revenue share as a distribution engine.** No competitor can copy it cheaply. The level and the
  conditions need work (finding 6), the idea does not.
- **One contract for custody, risk and settlement**, with tighten-only admin.
- **Oracle-priced fills with no book impact** on tokens that have no perp anywhere.

## Findings, ranked

Severity: **A** = funds can be taken or trapped at the scale of a public launch; **B** = the venue does not work
economically or users cannot do what the product promises; **C** = missing table stakes.

### 1. The listing surface is unbounded (A)

A permissionless lister currently chooses almost everything that protects other people's money.

| What | Evidence |
| --- | --- |
| The price source is any contract the lister names. `on_price_read` trusts whatever reserves that contract returns. There is no DEX allowlist and no check that the pool holds the market's token. | `model.rs:84-88`, `contract.rs:806-867` |
| `base_decimals` is lister input and unchecked, so a real pool can be mispriced by a power of ten. | `contract.rs:403`, HANDOFF gap 5 |
| `MarketParams` (margins, liquidation fee and cap, staircase, funding cap, warmup), `spread_bps` and `max_oracle_age` are lister input. The only wrapper check is spread ≤ 10%. | `contract.rs:398-430` |
| OI caps start at 0, which means uncapped, and only the owner can set them. The spec formula is not computed anywhere. The leverage ratchet exists and is unused. | `contract.rs:458-460`, `:1316`, `policy.rs` |
| The LP pool account can itself be liquidated, paying the caller 60% of the fee. | `contract.rs:1556-1569`, `:1812` |
| `symbol` is a free string with no uniqueness check. | `contract.rs:437` |

Worked example: deploy a 30-line contract that answers `get_pool` with any reserves, list a market against it,
wait for deposits, walk the price, liquidate everyone into a pool you own. Cost: gas.

Even with a real pool, the long-tail depth floor is 1,000 USDC of quote reserve (`contract.rs:57`). Moving such a
pool 19% takes about 95 USDC of inventory.

Change: an owner-set DEX allowlist; pool pair and decimals read on-chain from the pool and the token's metadata;
parameters picked from protocol tiers, never free-form; OI caps computed on-chain at every observation and LP
change; the pool account excluded from `liquidate`; market identity is (token, DEX, pool), ticker read from the
token. Most of this is already what `docs/contract.md` §5 and D9 say ("if on-chain rules pass"), so it reverses
nothing. The cap formula is proposal P3.

### 2. The price can be both farmed and frozen (A)

For `NearPool` markets the fill price is an EMA of one pool's spot reserves, 5% weight per **sample**
(`policy.rs:186`, `contract.rs:895-907`). `refresh_price` is permissionless with no minimum interval
(`contract.rs:806`).

- **Farmed.** The next mark is a published forecast: 0.95 × EMA + 0.05 × spot. Spot jumps 5%, a trader opens at
  the old mark plus about 25 bps, and the mark converges in about 60 samples. That is roughly 4% of notional paid
  by the LPs, 16% on margin at 4x. On a 100k USD pool, 100k of such flow at an average 3% lag costs about five
  days of LP income. The two-step order only stops prices older than the request.
- **Steered.** Because the weight is per sample and samples are not limited to one per block, whoever spams
  `refresh_price` while the pool is pushed controls how fast the mark moves. About 45 refreshes close 90% of a
  gap. Moving a constant-product pool with quote reserve X by 20% takes about 0.095 X of capital and costs at
  most about 0.8% of X if fully arbitraged, far less if unwound within two blocks. A position is therefore
  profitable to manipulate once it exceeds roughly 0.6% of X, and liquidation hunting pays once 10x open
  interest on one side exceeds about 1.9% of X (risk seat's arithmetic).
- **Pushed prices have no band at all.** `push_price` checks order and age, not size of move
  (`contract.rs:1473-1492`). A leaked oracle key can walk a major at the staircase rate and liquidate every 10x
  account. This matters for the signed-median plan in P1.
- **Frozen.** A read more than 20% from the EMA flips the market to ReduceOnly and returns **before** writing the
  observation or the EMA (`contract.rs:871-892`). `execute_order` needs an observation newer than the request
  (`:663-666`), so from that block no new close can fill. `liquidate` keeps working at the frozen price for up to
  an hour (`:1561-1566`). After an hour nothing fills. There is no path back to Active (`:1340-1346`, audit M-1).
  A genuine re-rating of a memecoin by 25% does this with no attacker. An attacker can do it to a competitor's
  market for about 9.5% of the pool's quote reserve held for a block or two.

Change: proposal P2 (time-weighted mark, one sample per block, taker fills at the worse of spot and mark on
single-pool sources, liquidations on the mark only) and proposal P4 (band breach keeps recording, automatic
return to Active).

### 3. Users cannot always get out, or get paid (A/B)

- **Every trade is refused while the engine price is catching up to the target**, closes and stops included
  (`exchange.rs:360`, `:387`; `crates/SECURITY.md` W-7). On pushed or TEE markets, where the raw price is the
  target, a 10% move at 10 bps per block is about 100 blocks, roughly a minute with no exit. On `NearPool`
  markets the EMA softens this to a few blocks per refresh.
- **Profit from a keeper-filled take-profit can be stranded.** Profit must be converted within about `h_max`
  (50 blocks, about 30 s, in the tests) of maturing or its backing expires. Only the account owner can call
  `convert_pnl`; `convert_pnl_for` does not exist (`contract.rs:1373`, HANDOFF gap 3, `crates/SECURITY.md` F-1).
  D14 cannot work as written.
- **LPs can run.** Redemption is instant at the lower NAV bound (`contract.rs:1429-1455`, audit L-3). The target
  lags spot, so an LP watching the reference pool leaves before the loss is marked, and redemption then fails for
  everyone else once the pool's margin binds.

Change: proposal P4 (reduce-only fills during lag at the worse price for the taker; conversion inside the fill
or by keepers) and the 24 h redeem delay the spec already calls for.

### 4. LP economics do not work, so there is no seed capital (B)

- **The LP's 30% of fees is never paid.** `accrue_fee_split` writes a ledger, `claim_fees` rejects `Payee::Lp`,
  and NAV does not include it (`contract.rs:1752-1775`, `:1268-1269`, `policy.rs:129`).
- **With caps unset the pool is a 4x to 10x short-volatility book.** A 100k pool can carry 400k of net long-tail
  open interest. The pool is under maintenance margin after an 8.7% move at 4x, or 1.85% at 10x, and is then
  partially liquidated, paying the 5% fee out of LP capital. A realistic loss on a 25% move is about a third of
  TVL plus fees; on a 5x day LPs lose everything and winners are haircut to a few percent. Percolator guarantees
  the vault never pays out more than it holds. It does not protect the LP's seed, and it does not protect anyone
  from a bad price.
- **Nobody is designated to seed a permissionless market**, and `activate_market` requires no LP capital.
- Insurance earns 0.5 bps of volume, 50 USD a day per 1M traded, per market, starting from zero.
- An LP cannot see pool net position, OI by side, utilisation, haircut ratio or insurance balance.

Change: credit the LP share to pool capital on each fill (implements D11 as written); proposals P3 and P5; add
the missing views.

### 5. Majors are priced out of the market and "10x" is not really 10x (B)

A 50k USD SOL round trip at 10x held four hours, with a moderately skewed book, costs about 72 bps on Neap
against roughly 10 bps on Hyperliquid and 15 bps on Jupiter (reviewers' estimates). Three design causes:

- Closes are charged the full widening spread because `compute_spread` adds a closing order's size to its own
  side instead of netting it (`contract.rs:1730-1740`, audit M-6). The spec says skew-reducing trades pay only
  `spread_min`.
- Funding is `cap × |pool net| / OI` (`policy.rs:62`). It ignores pool size, so one 10 USDC long alone in a 100k
  pool pays the cap. At the documented cap that is about 1.44% a day, and the cap itself is lister input.
- IM 10% with MM 8% and a 5% liquidation fee puts liquidation about 1.7% from entry after costs. The spec's own
  note says: if 10x feels tight, lower the liquidation fee, not the envelope. The envelope today leaves 0.9% of
  slack, about 5 s, while the keeper's liquidation sweep runs every 15 s at best.
- A 55/45 book pays about 95% a year in funding, far above incumbents in calm markets, while the cap is far
  below what a memecoin frenzy clears at. The shape is wrong at both ends.

Change: proposal P6 (liquidation terms and funding).

### 6. The fee schedule pays the wrong parties and cannot fund the protocol (B)

At 5 bps the protocol keeps 0.5 bps a side: about 50 USD a day at 1M of daily volume, and about 1.8M a year at
100M a day. The lister earns 2.5 bps with no capital at risk; the LP carries the whole tail for a nominal
1.5 bps. Under any sane cap a long-tail lister earns a few dollars a day, so the honest deal does not pay either.
The "bond" is credited to the lister's own wallet and can be withdrawn at once (`contract.rs:470-473`), and
`CreateMarket` pays no storage, so spam markets consume the contract's own NEAR.

Change: proposal P5 (fee schedule, locked first-loss bond, Unverified and Verified tiers).

### 7. Trading experience gaps against table stakes (C)

- **No one-click trading.** `request_order` requires 1 yoctoNEAR, which forces a full-access-key signature and a
  wallet prompt per order. A round trip is five to seven signed transactions. The one-pager promises one-click
  trading through access keys. Change: `authorize_agent(pubkey, expiry, notional_limit)` for request, cancel and
  conditional orders only, never withdrawals (proposal P7).
- **Fills are all-or-nothing** against the cap; limits are triggers that then pay taker costs; a wick between
  keeper polls is missed; one keeper polls conditionals at 3 s or more and liquidations at 15 s or more
  (`services/keeper/src/keeper.rs:341`); liquidation discovery depends on the indexer (HANDOFF gap 1).
- **The haircut is invisible.** A trader should see how much of open profit the pool can pay before opening.
- `set_oi_caps` lets the owner raise caps as well as lower them (`contract.rs:1316`), which does not match D15's
  "tighten-only" admin.
- No status page, no account enumeration view, no wind-down path: `Recovery`, `WindDown` and `Closed` exist in
  the enum with no transitions (`model.rs:66-74`).

### 8. The launch slate and phase order aim at nobody (B, strategic)

SOL at 10x with a 2 to 4 s fill loses to every incumbent on cost. NEAR tokens deep enough to list and worth
shorting are few. The first real user is a lister, a token community or a trader with a following on Solana, and
then their followers. Both headline claims are "coming soon" in the app today. All five seats backed going
Solana-token-first, with the price as a signed median over several pools so that markets are not capped by one
thin pool. This reverses D5 and D17 and is proposal P1.

"Any chain" on its own is not a wedge; cross-chain deposits are common. "Any token, and the lister earns" is.

## How the disputes were settled

| # | Dispute | Outcome |
| --- | --- | --- |
| Q1 | Fee level and split | Agreed: 5 bps cannot pay for long-tail risk, and majors are too dear. All seats landed within a few points of each other. See P5. The trader's condition holds: a higher long-tail fee replaces the minimum spread, it does not stack on it. |
| Q2 | Mark price | Four of five seats ended on dual pricing over a time-weighted mark. The competitor withdrew "fill at fresh spot" after the lister showed spot of one thin pool is what a malicious lister controls. Open: the trader wants closes at fresh spot. The chair does not recommend that on single-pool sources (open at the mark, push spot, close at spot). Revisit when prices are a multi-pool median. |
| Q3 | OI cap | Agreed that 2% of depth makes markets toy-sized and that two caps are needed: gross per side against reference depth, net against pool NAV. Range offered: 5 to 10% of depth for gross, with the risk seat holding at 5% for a single-pool source and arguing caps should grow with the quality of the price source, not with ambition. See P3. |
| Q4 | Liquidation terms | Agreed for majors: MM 5% and fee 1% fit the envelope (2% + 0.5% + 1% = 3.5%), on condition the keeper sweep drops from 15 s to 3 s or less. The risk seat's arithmetic: 2.0% move + 1.02% fee leaves 1.98% of slack (about 12 s) against 0.9% today, and still fits with a 28-block keeper delay. Long tail: three seats for MM 15% with a 2 to 2.5% fee, two for MM 10% with fee 2%. |
| Q5 | Listing friction | Agreed: keep the 60-second listing as an Unverified tier with hard limits, and a Verified tier above it. See P5. |
| Q6 | Sequencing | Agreed: Solana tokens first, on a signed multi-pool median. Conditions from the LP and lister: at least 2-of-3 signers, low caps, and curated listing while the signer is trusted. |
| Q7 | Pool structure | Unanimous: keep isolated pools. An opt-in allocator vault over protocol-listed markets can come later. |

Corrections made during cross-examination: wash trading costs a lister about 250 USD per 1M of volume, not 100;
the "no exit for 20 s" figure applies to pushed and TEE markets rather than `NearPool` ones; the band-breach
finding was worse than first stated (closes fail immediately, liquidations continue); "a 25% move wipes the
pool" was overstated, since the pool is partially liquidated first.

One item is a code defect rather than a design gap and is passed to the contract audit: the liquidator's payout
leaves the custody vault without a matching engine insurance withdrawal (`contract.rs:1580`, compare the fix in
`claim_fees` at `:1281`), so custody may drift below the engine vault with each liquidation. Not verified here.

## The shortest list

In order. Items 1 to 4 need no founder decision reversed; they implement the existing spec or fix a gap.

1. **Bound the listing surface**: DEX allowlist, pair and decimals checked on-chain, tier parameters, computed OI
   caps, pool not liquidatable, market identity.
2. **Fix the mark**: time-weighted, one sample per block, worse-of pricing for the taker.
3. **Make exits and payouts unconditional**: band recovery, reduce-only fills during lag, conversion without the
   owner online, LP redeem delay.
4. **Pay LPs**: credit the fee share to NAV, add LP views.
5. **Reprice**: fee schedule by tier, locked first-loss lister bond, liquidation terms, utilisation-based funding.
6. **Session keys** for orders.
7. **Re-aim the launch**: Solana tokens first on a signed median.
8. **Operate it like a venue**: second keeper, status page, wind-down path, haircut and backing shown on the
   ticket.
