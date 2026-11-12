//! Mock Ref Finance pool contract for sandbox tests.
//! Returns a configurable `get_pool` response that matches the shape
//! `on_price_read` expects from `v2.ref-finance.near`.

use near_sdk::{env, near, require, serde_json, AccountId, PanicOnDefault};

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct MockRefPool {
    /// Token account IDs in the pool (e.g. ["usdc.near", "token.near"]).
    token_account_ids: Vec<AccountId>,
    /// Reserve amounts as strings (matches Ref Finance JSON).
    amounts: Vec<String>,
}

#[near]
impl MockRefPool {
    #[init]
    pub fn new(token_account_ids: Vec<AccountId>, amounts: Vec<String>) -> Self {
        require!(token_account_ids.len() == 2, "need exactly 2 tokens");
        require!(amounts.len() == 2, "need exactly 2 amounts");
        Self {
            token_account_ids,
            amounts,
        }
    }

    /// Set new reserve amounts (for simulating price changes).
    pub fn set_amounts(&mut self, amounts: Vec<String>) {
        require!(amounts.len() == 2, "need exactly 2 amounts");
        self.amounts = amounts;
    }

    /// Ref Finance `get_pool` shape. The contract ignores `pool_id` — there's
    /// only one pool in this mock.
    pub fn get_pool(&self, pool_id: u64) -> serde_json::Value {
        let _ = pool_id;
        serde_json::json!({
            "pool_kind": "SIMPLE_POOL",
            "token_account_ids": self.token_account_ids,
            "amounts": self.amounts,
            "total_fee": 30,
            "shares_total_supply": "1000000000000000000000000",
        })
    }
}
