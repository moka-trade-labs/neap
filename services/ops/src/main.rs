//! neap-ops: tiny NEAR ops CLI (no near-cli needed).
//!
//!   neap-ops keygen
//!   neap-ops call     <signer> <receiver> <method> '<json>' [deposit_yocto] [tgas]
//!   neap-ops deploy   <signer> <wasm path> [init_method '<json>']
//!   neap-ops sub      <signer> <new_account> <near_amount> <public_key>   (create subaccount)
//!   neap-ops transfer <signer> <to> <near_amount>
//!   neap-ops view     <account> <method> '<json>'
//!
//! Env: NEAR_RPC_URL (default testnet fastnear), NEAR_KEY (ed25519:... of the signer).

use anyhow::{anyhow, Context, Result};
use near_crypto::{KeyType, SecretKey};
use near_primitives::action::{
    Action, AddKeyAction, CreateAccountAction, DeployContractAction, FunctionCallAction, TransferAction,
};
use near_primitives::account::AccessKey;
use near_primitives::hash::CryptoHash;
use near_primitives::transaction::{SignedTransaction, Transaction, TransactionV0};
use near_primitives::types::AccountId;
use serde_json::{json, Value};
use std::str::FromStr;

fn rpc_url() -> String {
    std::env::var("NEAR_RPC_URL").unwrap_or_else(|_| "https://test.rpc.fastnear.com".into())
}

async fn rpc(method: &str, params: Value) -> Result<Value> {
    let resp: Value = reqwest::Client::new()
        .post(rpc_url())
        .json(&json!({"jsonrpc":"2.0","id":"ops","method":method,"params":params}))
        .timeout(std::time::Duration::from_secs(90))
        .send()
        .await?
        .json()
        .await?;
    if let Some(e) = resp.get("error") {
        return Err(anyhow!("rpc error: {e}"));
    }
    Ok(resp["result"].clone())
}

fn yocto(near: &str) -> Result<u128> {
    let (i, f) = near.split_once('.').unwrap_or((near, ""));
    let mut f = f.to_string();
    while f.len() < 24 {
        f.push('0');
    }
    Ok(i.parse::<u128>()? * 10u128.pow(24) + f[..24].parse::<u128>()?)
}

async fn send(signer: &str, receiver: &str, actions: Vec<Action>) -> Result<Value> {
    let sk = SecretKey::from_str(&std::env::var("NEAR_KEY").context("NEAR_KEY required")?)?;
    let pk = sk.public_key();
    let ak = rpc("query", json!({"request_type":"view_access_key","finality":"final",
        "account_id":signer,"public_key":pk.to_string()})).await?;
    let nonce = ak["nonce"].as_u64().context("nonce")? + 1;
    let block_hash = CryptoHash::from_str(ak["block_hash"].as_str().context("hash")?).map_err(|e| anyhow!("{e}"))?;
    let tx = Transaction::V0(TransactionV0 {
        signer_id: signer.parse()?,
        public_key: pk,
        nonce,
        receiver_id: receiver.parse()?,
        block_hash,
        actions,
    });
    let h = CryptoHash::hash_borsh(&tx);
    let signed = SignedTransaction::new(sk.sign(h.as_ref()), tx);
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(borsh::to_vec(&signed)?);
    let r = rpc("send_tx", json!({"signed_tx_base64": b64, "wait_until": "FINAL"})).await?;
    let status = &r["status"];
    let hash = r["transaction"]["hash"].as_str().unwrap_or("?");
    // Surface receipt failures and the final return value.
    let mut fails = vec![];
    for ro in r["receipts_outcome"].as_array().into_iter().flatten() {
        if let Some(f) = ro["outcome"]["status"].get("Failure") {
            fails.push(f.clone());
        }
    }
    let ret = status
        .get("SuccessValue")
        .and_then(|v| v.as_str())
        .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
        .map(|b| String::from_utf8_lossy(&b).to_string());
    let out = json!({"tx": hash, "status_failure": status.get("Failure"), "receipt_failures": fails, "return": ret});
    if status.get("Failure").is_some() {
        return Err(anyhow!("tx failed: {out}"));
    }
    Ok(out)
}

fn fcall(method: &str, args: &str, deposit: u128, tgas: u64) -> Result<Action> {
    let v: Value = serde_json::from_str(args).context("args must be JSON")?;
    Ok(Action::FunctionCall(Box::new(FunctionCallAction {
        method_name: method.into(),
        args: serde_json::to_vec(&v)?,
        gas: tgas * 1_000_000_000_000,
        deposit,
    })))
}

#[tokio::main]
async fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| a.get(i).cloned().ok_or_else(|| anyhow!("missing arg {i}"));
    let out = match a.first().map(String::as_str) {
        Some("keygen") => {
            let sk = SecretKey::from_random(KeyType::ED25519);
            let pk = sk.public_key();
            let implicit = hex(&pk.key_data());
            json!({"public_key": pk.to_string(), "secret_key": sk.to_string(), "implicit_account": implicit})
        }
        Some("view") => {
            use base64::Engine;
            let args: Value = serde_json::from_str(&a.get(3).cloned().unwrap_or("{}".into()))?;
            let r = rpc("query", json!({"request_type":"call_function","finality":"final",
                "account_id":arg(1)?,"method_name":arg(2)?,
                "args_base64": base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&args)?)})).await?;
            let bytes: Vec<u8> = r["result"].as_array().context("no result")?.iter().map(|x| x.as_u64().unwrap_or(0) as u8).collect();
            serde_json::from_slice(&bytes).unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()))
        }
        Some("call") => {
            let dep: u128 = a.get(5).map(|s| s.parse()).transpose()?.unwrap_or(0);
            let tgas: u64 = a.get(6).map(|s| s.parse()).transpose()?.unwrap_or(100);
            send(&arg(1)?, &arg(2)?, vec![fcall(&arg(3)?, &arg(4)?, dep, tgas)?]).await?
        }
        Some("deploy") => {
            let code = std::fs::read(arg(2)?)?;
            let mut acts = vec![Action::DeployContract(DeployContractAction { code })];
            if let (Some(m), Some(j)) = (a.get(3), a.get(4)) {
                acts.push(fcall(m, j, 0, 200)?);
            }
            send(&arg(1)?, &arg(1)?, acts).await?
        }
        Some("sub") => {
            let pk = near_crypto::PublicKey::from_str(&arg(4)?)?;
            send(&arg(1)?, &arg(2)?, vec![
                Action::CreateAccount(CreateAccountAction {}),
                Action::Transfer(TransferAction { deposit: yocto(&arg(3)?)? }),
                Action::AddKey(Box::new(AddKeyAction { public_key: pk, access_key: AccessKey::full_access() })),
            ]).await?
        }
        Some("transfer") => {
            let _to: AccountId = arg(2)?.parse()?;
            send(&arg(1)?, &arg(2)?, vec![Action::Transfer(TransferAction { deposit: yocto(&arg(3)?)? })]).await?
        }
        _ => return Err(anyhow!("usage: see the doc comment in services/ops/src/main.rs")),
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
