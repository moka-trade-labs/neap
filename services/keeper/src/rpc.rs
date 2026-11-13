//! Minimal NEAR JSON-RPC client for the keeper.
//! Handles view calls, sending signed transactions, and nonce management.

use anyhow::{anyhow, Context, Result};
use near_crypto::InMemorySigner;
use near_primitives::action::{Action, FunctionCallAction};
use near_primitives::hash::CryptoHash;
use near_primitives::types::{AccountId, Gas};
use serde_json::{json, Value};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

const CALL_GAS: Gas = 80_000_000_000_000; // 80 TGas

pub struct NearRpc {
    client: reqwest::Client,
    rpc_urls: Vec<String>,
    rpc_idx: std::sync::atomic::AtomicUsize,
    contract_id: AccountId,
    signer_id: AccountId,
    signer: InMemorySigner,
    nonce: AtomicU64,
    block_hash: std::sync::Mutex<CryptoHash>,
}

impl NearRpc {
    pub async fn new(
        rpc_url: &str,
        contract_id: &str,
        signer_id: &str,
        signer_key: &str,
    ) -> Result<Self> {
        let contract_id: AccountId = contract_id.parse().context("bad contract_id")?;
        let signer_account: AccountId = signer_id.parse().context("bad signer_id")?;

        let secret_key = near_crypto::SecretKey::from_str(signer_key)
            .context("bad signer key")?;
        let public_key = secret_key.public_key();
        let signer = InMemorySigner {
            account_id: signer_account.clone(),
            public_key,
            secret_key,
        };

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .build()?;
        let rpc = Self {
            client,
            rpc_urls: rpc_url.split(',').map(|u| u.trim().to_string()).filter(|u| !u.is_empty()).collect(),
            rpc_idx: Default::default(),
            contract_id,
            signer_id: signer_account,
            signer,
            nonce: AtomicU64::new(0),
            block_hash: std::sync::Mutex::new(CryptoHash::default()),
        };
        rpc.refresh_access_key().await?;
        Ok(rpc)
    }

    /// Refresh nonce and block hash from the network.
    pub async fn refresh_access_key(&self) -> Result<()> {
        let pk = self.signer.public_key.to_string();
        let resp = self.rpc_call("query", json!({
            "request_type": "view_access_key",
            "finality": "final",
            "account_id": self.signer_id.to_string(),
            "public_key": pk,
        })).await?;
        let nonce = resp["nonce"].as_u64().context("no nonce")?;
        self.nonce.store(nonce, Ordering::SeqCst);

        let hash_str = resp["block_hash"].as_str().context("no block_hash")?;
        let hash = CryptoHash::from_str(hash_str)
            .map_err(|e| anyhow!("bad block_hash: {e}"))?;
        *self.block_hash.lock().unwrap() = hash;
        Ok(())
    }

    /// View call (free, no gas).
    pub async fn view(&self, method: &str, args: Value) -> Result<Value> {
        let args_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            serde_json::to_vec(&args)?,
        );
        let resp = self.rpc_call("query", json!({
            "request_type": "call_function",
            "finality": "optimistic",
            "account_id": self.contract_id.to_string(),
            "method_name": method,
            "args_base64": args_b64,
        })).await?;
        let result_bytes: Vec<u8> = resp["result"]
            .as_array()
            .context("no result array")?
            .iter()
            .map(|v| v.as_u64().unwrap_or(0) as u8)
            .collect();
        let parsed: Value = serde_json::from_slice(&result_bytes)
            .unwrap_or(Value::Null);
        Ok(parsed)
    }

    /// Send a function call transaction to the neap contract. Returns the tx hash.
    pub async fn call(
        &self,
        method: &str,
        args: Value,
        deposit: u128,
    ) -> Result<String> {
        let contract = self.contract_id.clone();
        self.call_many(&contract, vec![(method.to_string(), args, deposit)], CALL_GAS)
            .await
    }

    /// Send one transaction with several function calls to any receiver.
    pub async fn call_many(
        &self,
        receiver: &AccountId,
        calls: Vec<(String, Value, u128)>,
        gas_each: Gas,
    ) -> Result<String> {
        let nonce = self.nonce.fetch_add(1, Ordering::SeqCst) + 1;
        let block_hash = *self.block_hash.lock().unwrap();
        let method = calls.first().map(|c| c.0.clone()).unwrap_or_default();

        let mut actions = Vec::with_capacity(calls.len());
        for (m, args, deposit) in calls {
            actions.push(Action::FunctionCall(Box::new(FunctionCallAction {
                method_name: m,
                args: serde_json::to_vec(&args)?,
                gas: gas_each,
                deposit,
            })));
        }

        // Build the transaction struct, then sign it.
        let tx = near_primitives::transaction::Transaction::V0(
            near_primitives::transaction::TransactionV0 {
                signer_id: self.signer_id.clone(),
                public_key: self.signer.public_key.clone(),
                nonce,
                receiver_id: receiver.clone(),
                block_hash,
                actions,
            },
        );
        let tx_hash = CryptoHash::hash_borsh(&tx);
        let signature = self.signer.secret_key.sign(tx_hash.as_ref());
        let signed =
            near_primitives::transaction::SignedTransaction::new(signature, tx);
        let tx_bytes = borsh::to_vec(&signed)?;
        let tx_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &tx_bytes,
        );

        let resp = match self.send_tx(&tx_b64).await {
            Ok(r) => r,
            Err(e) => {
                // Nonce/blockhash problems: resync so the next call works.
                let es = e.to_string();
                if es.contains("InvalidNonce") || es.contains("Expired") || es.contains("InvalidTxError") {
                    let _ = self.refresh_access_key().await;
                }
                return Err(e);
            }
        };
        if let Some(err) = resp.get("status").and_then(|s| s.get("Failure")) {
            return Err(anyhow!("{method} tx failed: {err}"));
        }
        let hash = resp["transaction"]["hash"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        Ok(hash)
    }

    /// Send plain NEAR.
    pub async fn transfer(&self, to: &AccountId, yocto: u128) -> Result<String> {
        let nonce = self.nonce.fetch_add(1, Ordering::SeqCst) + 1;
        let block_hash = *self.block_hash.lock().unwrap();
        let tx = near_primitives::transaction::Transaction::V0(
            near_primitives::transaction::TransactionV0 {
                signer_id: self.signer_id.clone(),
                public_key: self.signer.public_key.clone(),
                nonce,
                receiver_id: to.clone(),
                block_hash,
                actions: vec![Action::Transfer(near_primitives::action::TransferAction { deposit: yocto })],
            },
        );
        let tx_hash = CryptoHash::hash_borsh(&tx);
        let signature = self.signer.secret_key.sign(tx_hash.as_ref());
        let signed = near_primitives::transaction::SignedTransaction::new(signature, tx);
        let tx_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, borsh::to_vec(&signed)?);
        let resp = self.send_tx(&tx_b64).await?;
        Ok(resp["transaction"]["hash"].as_str().unwrap_or("unknown").to_string())
    }

    /// Whether an account exists on chain.
    pub async fn account_exists(&self, account: &str) -> Result<bool> {
        match self
            .rpc_call("query", json!({
                "request_type": "view_account",
                "finality": "final",
                "account_id": account,
            }))
            .await
        {
            Ok(_) => Ok(true),
            Err(e) if e.to_string().contains("UNKNOWN_ACCOUNT") || e.to_string().contains("does not exist") => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub fn signer_id(&self) -> &AccountId {
        &self.signer_id
    }

    /// Broadcast a signed tx and wait for execution. A rate-limited or
    /// unsupported RPC rotates and the same signed tx is sent to the next one
    /// (same hash, so a duplicate is harmless).
    async fn send_tx(&self, tx_b64: &str) -> Result<Value> {
        let n = self.rpc_urls.len().max(1);
        // INCLUDED returns in about a second, which free RPC plans tolerate. The
        // keeper is idempotent, so a tx that later fails is simply retried next tick.
        let wait = std::env::var("NEAP_TX_WAIT").unwrap_or_else(|_| "INCLUDED".to_string());
        let params = json!({ "signed_tx_base64": tx_b64, "wait_until": wait });
        let mut last = anyhow!("no rpc configured");
        for _ in 0..n {
            let idx = self.rpc_idx.load(Ordering::Relaxed) % n;
            match self.rpc_call_on(&self.rpc_urls[idx], "send_tx", &params).await {
                Ok(v) => return Ok(v),
                Err((true, e)) => {
                    self.rpc_idx.store((idx + 1) % n, Ordering::Relaxed);
                    last = e;
                }
                Err((false, e)) => return Err(e),
            }
        }
        Err(last)
    }

    /// JSON-RPC call. Rate limits and transport errors rotate to the next
    /// RPC; queries are retried there, transactions are not re-sent.
    pub async fn rpc_call(&self, method: &str, params: Value) -> Result<Value> {
        let n = self.rpc_urls.len().max(1);
        let tries = if method == "query" { n } else { 1 };
        let mut last = anyhow!("no rpc configured");
        for _ in 0..tries {
            let idx = self.rpc_idx.load(Ordering::Relaxed) % n;
            match self.rpc_call_on(&self.rpc_urls[idx], method, &params).await {
                Ok(v) => return Ok(v),
                Err((rotate, e)) => {
                    if rotate {
                        self.rpc_idx.store((idx + 1) % n, Ordering::Relaxed);
                    } else {
                        return Err(e);
                    }
                    last = e;
                }
            }
        }
        Err(last)
    }

    async fn rpc_call_on(&self, url: &str, method: &str, params: &Value) -> std::result::Result<Value, (bool, anyhow::Error)> {
        let body = json!({ "jsonrpc": "2.0", "id": "keeper", "method": method, "params": params });
        let mut req = self.client.post(url).json(&body);
        if url.contains("fastnear") {
            if let Ok(k) = std::env::var("FASTNEAR_API_KEY") {
                if !k.trim().is_empty() {
                    req = req.bearer_auth(k.trim());
                }
            }
        }
        let resp = req.send().await.map_err(|e| (true, e.into()))?;
        let limited = resp.status().as_u16() == 429 || resp.status().is_server_error();
        let resp: Value = resp.json().await.map_err(|e| (true, e.into()))?;
        if let Some(err) = resp.get("error") {
            let es = err.to_string();
            let rotate = limited
                || es.contains("429")
                || es.contains("Rate limit")
                || es.contains("rate limit")
                || es.contains("-32601")
                || es.contains("timeout")
                || es.contains("not available");
            return Err((rotate, anyhow!("RPC error on {method}: {err}")));
        }
        if limited {
            return Err((true, anyhow!("RPC {url} unavailable for {method}")));
        }
        resp.get("result")
            .cloned()
            .ok_or_else(|| (true, anyhow!("no result in RPC response for {method}")))
    }
}
