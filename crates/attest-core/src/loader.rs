//! Builds claims from the JSON written by eth/scripts/fetch-proofs.mjs.

use crate::*;
use alloy_primitives::{Address, Bytes, B256, U256};
use serde_json::Value;
use std::{fs, path::Path};

fn read(dir: &Path, f: &str) -> Result<Value, String> {
    let s = fs::read_to_string(dir.join(f)).map_err(|e| format!("{f}: {e}"))?;
    serde_json::from_str(&s).map_err(|e| format!("{f}: {e}"))
}

fn hex_u256(v: &Value) -> Result<U256, String> {
    let s = v.as_str().ok_or("expected hex string")?;
    U256::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| e.to_string())
}

fn de<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T, String> {
    serde_json::from_value(v.clone()).map_err(|e| e.to_string())
}

pub fn account_from_get_proof(v: &Value) -> Result<(AccountProof, Vec<StorageProof>), String> {
    let account = AccountProof {
        address: de::<Address>(&v["address"])?,
        nonce: hex_u256(&v["nonce"])?.to::<u64>(),
        balance: hex_u256(&v["balance"])?,
        storage_hash: de::<B256>(&v["storageHash"])?,
        code_hash: de::<B256>(&v["codeHash"])?,
        proof: de::<Vec<Bytes>>(&v["accountProof"])?,
    };
    let mut storage = Vec::new();
    for sp in v["storageProof"].as_array().cloned().unwrap_or_default() {
        storage.push(StorageProof {
            key: B256::from(hex_u256(&sp["key"])?),
            value: hex_u256(&sp["value"])?,
            proof: de::<Vec<Bytes>>(&sp["proof"])?,
        });
    }
    Ok((account, storage))
}

pub struct Loaded {
    pub claim: Claim,
    pub finalized_beacon_root: B256,
    pub expected_balance: U256,
    pub meta: Value,
}

/// token.balances[holder] >= rhs
pub fn load_token_balance_claim(dir: &Path, rhs: U256) -> Result<Loaded, String> {
    let meta = read(dir, "meta.json")?;
    let fu = read(dir, "finality_update.json")?;
    let fin = &fu["data"]["finalized_header"];
    let beacon_header: BeaconBlockHeader = de(&fin["beacon"])?;
    let branch: Vec<B256> = de(&fin["execution_branch"])?;
    let execution = if fin.get("execution").is_some() {
        ExecutionAnchor::PayloadHeader { execution: de(&fin["execution"])?, branch }
    } else if fin.get("execution_block_hash").is_some() {
        ExecutionAnchor::BlockHash { block_hash: de(&fin["execution_block_hash"])?, branch }
    } else {
        return Err("finalized_header has neither `execution` nor `execution_block_hash`".into());
    };
    let exec = read(dir, "exec_header.json")?;
    let exec_header_rlp: Bytes = de(&exec["rlp"])?;
    let (account, mut storage) = account_from_get_proof(&read(dir, "proof_token.json")?)?;
    let slot: B256 = B256::from(hex_u256(&meta["storage_key"])?);
    let sp = storage.drain(..).find(|s| s.key == slot).ok_or("storage proof for meta.storage_key missing")?;
    let chain_id: u64 = meta["chain_id"].as_str().and_then(|s| s.parse().ok()).or(meta["chain_id"].as_u64()).ok_or("meta.chain_id")?;
    let expected_balance = U256::from_str_radix(meta["expected_balance"].as_str().ok_or("meta.expected_balance")?, 10).map_err(|e| e.to_string())?;
    let finalized_beacon_root = beacon_header.hash_tree_root();
    Ok(Loaded {
        claim: Claim {
            chain_id,
            beacon_header,
            execution,
            exec_header_rlp,
            account,
            storage: Some(sp),
            predicate: Predicate { subject: Subject::StorageSlot(slot), cmp: Cmp::Gte, rhs },
        },
        finalized_beacon_root,
        expected_balance,
        meta,
    })
}

/// DESCROW milestone 0: escrowedAmount == expected and status == Funded.
pub fn load_descrow_claims(dir: &Path, base: &Claim) -> Result<Vec<(&'static str, Claim)>, String> {
    let meta = read(dir, "meta.json")?;
    let d = &meta["descrow"];
    if d.is_null() {
        return Err("meta.descrow missing: HOLDER was not the DESCROW deal".into());
    }
    let (deal, storage) = account_from_get_proof(&read(dir, "proof_deal.json")?)?;
    let mut out = Vec::new();
    for (label, key, rhs) in [
        ("escrowedAmount", "escrowedAmount", U256::from_str_radix(d["escrowed_amount"].as_str().ok_or("escrowed_amount")?, 10).map_err(|e| e.to_string())?),
        ("status == Funded", "status", U256::from(1u64)),
    ] {
        let slot = B256::from(hex_u256(&d["slots"][key])?);
        let sp = storage.iter().find(|s| s.key == slot).cloned().ok_or("deal storage proof missing")?;
        let mut c = base.clone();
        c.account = deal.clone();
        c.storage = Some(sp);
        c.predicate = Predicate { subject: Subject::StorageSlot(slot), cmp: Cmp::Eq, rhs };
        out.push((label, c));
    }
    Ok(out)
}
