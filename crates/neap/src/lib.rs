//! neap: perps on any token, settled on NEAR; a contract around the percolator v16 risk engine.
//!
//! Layering (each layer only depends on the ones above it):
//!
//! - `percolator` (../engine): the chain-agnostic, Kani-verified risk engine.
//! - [`policy`]: pure wrapper decisions (pool pricing, funding, oracle
//!   acceptance, LP shares, custody ledger). No engine, no near-sdk. Kani-proved.
//! - [`exchange`]: engine glue and spec-mandated sequencing (oracle catch-up,
//!   lag gating, pool-to-peer fills). No near-sdk. Unit-tested natively.
//! - `contract` (feature `contract`): near-sdk storage, auth, NEP-141 custody,
//!   async withdrawals.

extern crate alloc;

pub mod exchange;
pub mod policy;

#[cfg(feature = "contract")]
mod contract;
#[cfg(feature = "contract")]
pub mod model;
#[cfg(feature = "contract")]
pub use contract::*;

#[cfg(kani)]
mod proofs;
