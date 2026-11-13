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
    rpc_url: String,
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
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let rpc = Self {
            client,
            rpc_url: rpc_url.to_string(),
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
            "finality": "final",
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

        let resp = match self.rpc_call("broadcast_tx_commit", json!([tx_b64])).await {
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
        let resp = self.rpc_call("broadcast_tx_commit", json!([tx_b64])).await?;
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

    pub async fn rpc_call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": "keeper",
            "method": method,
            "params": params,
        });
        let resp: Value = self.client
            .post(&self.rpc_url)
            .json(&body)
            .send()
            .await?
            .json()
            .await?;
        if let Some(err) = resp.get("error") {
            return Err(anyhow!("RPC error on {method}: {err}"));
        }
        resp.get("result")
            .cloned()
            .ok_or_else(|| anyhow!("no result in RPC response for {method}"))
    }
}
