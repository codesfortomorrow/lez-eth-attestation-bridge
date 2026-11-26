//! Ethereum light client: checkpoint -> sync committee -> BLS-verified finality update.
//! Roots it accepts become the trusted set that attest-core checks claims against.

use alloy_primitives::{B256, U256};
use attest_core::{ssz, BeaconBlockHeader, TrustedRoots};
use blst::{min_pk::{PublicKey, Signature}, BLST_ERROR};
use sha2::{Digest, Sha256};

pub const SYNC_COMMITTEE_SIZE: usize = 512;
pub const BLS_DST: &[u8] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";
pub const DOMAIN_SYNC_COMMITTEE: [u8; 4] = [7, 0, 0, 0];
/// Gloas values were taken from live Sepolia data (see README, D-004). The fork is picked
/// by epoch, not by branch length, so a submitter can't point a branch at another field.
pub const FINALIZED_ROOT_GINDEX_ELECTRA: u64 = 169;
pub const CURRENT_SYNC_COMMITTEE_GINDEX_ELECTRA: u64 = 86;
pub const FINALIZED_ROOT_GINDEX_GLOAS: u64 = 735;
pub const CURRENT_SYNC_COMMITTEE_GINDEX_GLOAS: u64 = 2945;

/// Part of an instance's fixed config (353024 on Sepolia).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForkSchedule {
    pub gloas_epoch: Option<u64>,
}

impl ForkSchedule {
    pub fn is_gloas(&self, slot: u64) -> bool {
        self.gloas_epoch.is_some_and(|e| slot / 32 >= e)
    }
    pub fn finalized_root_gindex(&self, slot: u64) -> u64 {
        if self.is_gloas(slot) { FINALIZED_ROOT_GINDEX_GLOAS } else { FINALIZED_ROOT_GINDEX_ELECTRA }
    }
    pub fn current_sync_committee_gindex(&self, slot: u64) -> u64 {
        if self.is_gloas(slot) { CURRENT_SYNC_COMMITTEE_GINDEX_GLOAS } else { CURRENT_SYNC_COMMITTEE_GINDEX_ELECTRA }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LcError {
    #[error("L01 bootstrap header does not match the trusted checkpoint root")]
    CheckpointMismatch,
    #[error("L02 sync committee branch invalid")]
    CommitteeBranchInvalid,
    #[error("L03 sync committee participation below 2/3 ({0}/512)")]
    InsufficientParticipation(usize),
    #[error("L04 finality branch invalid")]
    FinalityBranchInvalid,
    #[error("L05 sync committee signature invalid")]
    SignatureInvalid,
    #[error("L06 signature period differs from the tracked committee period")]
    WrongPeriod,
    #[error("L07 malformed input: {0}")]
    Malformed(String),
}

#[derive(Debug, Clone)]
pub struct SyncCommittee {
    pub pubkeys: Vec<[u8; 48]>,
    pub aggregate_pubkey: [u8; 48],
}

fn pubkey_root(pk: &[u8; 48]) -> [u8; 32] {
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    a.copy_from_slice(&pk[..32]);
    b[..16].copy_from_slice(&pk[32..]);
    ssz::sha256_pair(&a, &b)
}

impl SyncCommittee {
    pub fn hash_tree_root(&self) -> [u8; 32] {
        let leaves: Vec<[u8; 32]> = self.pubkeys.iter().map(pubkey_root).collect();
        let pks = ssz::merkleize(&leaves, Some(SYNC_COMMITTEE_SIZE));
        ssz::sha256_pair(&pks, &pubkey_root(&self.aggregate_pubkey))
    }
}

pub fn compute_domain(domain_type: [u8; 4], fork_version: [u8; 4], genesis_validators_root: B256) -> [u8; 32] {
    let mut v = [0u8; 32];
    v[..4].copy_from_slice(&fork_version);
    let fork_data_root = ssz::sha256_pair(&v, &genesis_validators_root.0);
    let mut d = [0u8; 32];
    d[..4].copy_from_slice(&domain_type);
    d[4..].copy_from_slice(&fork_data_root[..28]);
    d
}

pub fn signing_root(object_root: B256, domain: [u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(object_root.0);
    h.update(domain);
    h.finalize().into()
}

#[derive(Debug, Clone)]
pub struct Store {
    pub chain_id: u64,
    pub forks: ForkSchedule,
    pub genesis_validators_root: B256,
    pub current_period: u64,
    pub current_committee: SyncCommittee,
    pub finalized_roots: Vec<B256>,
}

impl TrustedRoots for Store {
    fn chain_id(&self) -> u64 {
        self.chain_id
    }
    fn is_finalized(&self, r: &B256) -> bool {
        self.finalized_roots.contains(r)
    }
}

pub fn period_at_slot(slot: u64) -> u64 {
    slot / (32 * 256)
}

pub fn bootstrap(
    chain_id: u64,
    forks: ForkSchedule,
    genesis_validators_root: B256,
    checkpoint_root: B256,
    header: &BeaconBlockHeader,
    committee: SyncCommittee,
    committee_branch: &[B256],
) -> Result<Store, LcError> {
    if header.hash_tree_root() != checkpoint_root {
        return Err(LcError::CheckpointMismatch);
    }
    if !ssz::is_valid_gindex_branch(committee.hash_tree_root(), committee_branch, forks.current_sync_committee_gindex(header.slot), header.state_root) {
        return Err(LcError::CommitteeBranchInvalid);
    }
    Ok(Store {
        chain_id,
        forks,
        genesis_validators_root,
        current_period: period_at_slot(header.slot),
        current_committee: committee,
        finalized_roots: vec![checkpoint_root],
    })
}

pub struct FinalityUpdate {
    pub attested: BeaconBlockHeader,
    pub finalized: BeaconBlockHeader,
    pub finality_branch: Vec<B256>,
    pub sync_committee_bits: Vec<u8>, // 64 bytes, SSZ Bitvector[512] (little-endian bits)
    pub sync_committee_signature: [u8; 96],
    pub signature_slot: u64,
}

pub fn participants(bits: &[u8]) -> Vec<usize> {
    (0..SYNC_COMMITTEE_SIZE).filter(|i| bits.get(i / 8).is_some_and(|b| (b >> (i % 8)) & 1 == 1)).collect()
}

/// `fork_version` is the one active at epoch(signature_slot - 1).
pub fn apply_finality_update(store: &mut Store, u: &FinalityUpdate, fork_version: [u8; 4]) -> Result<B256, LcError> {
    let who = participants(&u.sync_committee_bits);
    if who.len() * 3 < SYNC_COMMITTEE_SIZE * 2 {
        return Err(LcError::InsufficientParticipation(who.len()));
    }
    if period_at_slot(u.signature_slot) != store.current_period {
        return Err(LcError::WrongPeriod);
    }
    let finalized_root = u.finalized.hash_tree_root();
    let gindex = store.forks.finalized_root_gindex(u.attested.slot);
    if !ssz::is_valid_gindex_branch(finalized_root.0, &u.finality_branch, gindex, u.attested.state_root) {
        return Err(LcError::FinalityBranchInvalid);
    }
    let domain = compute_domain(DOMAIN_SYNC_COMMITTEE, fork_version, store.genesis_validators_root);
    let msg = signing_root(u.attested.hash_tree_root(), domain);
    let pks: Vec<PublicKey> = who
        .iter()
        .map(|&i| PublicKey::from_bytes(&store.current_committee.pubkeys[i]))
        .collect::<Result<_, _>>()
        .map_err(|e| LcError::Malformed(format!("pubkey: {e:?}")))?;
    let refs: Vec<&PublicKey> = pks.iter().collect();
    let sig = Signature::from_bytes(&u.sync_committee_signature).map_err(|e| LcError::Malformed(format!("signature: {e:?}")))?;
    if sig.fast_aggregate_verify(true, &msg, BLS_DST, &refs) != BLST_ERROR::BLST_SUCCESS {
        return Err(LcError::SignatureInvalid);
    }
    if !store.finalized_roots.contains(&finalized_root) {
        store.finalized_roots.push(finalized_root);
    }
    Ok(finalized_root)
}

pub mod json {
    use super::*;
    use serde_json::Value;

    fn hex_bytes(v: &Value) -> Result<Vec<u8>, LcError> {
        let s = v.as_str().ok_or_else(|| LcError::Malformed("expected hex".into()))?;
        hex::decode(s.trim_start_matches("0x")).map_err(|e| LcError::Malformed(e.to_string()))
    }
    fn fixed<const N: usize>(v: &Value) -> Result<[u8; N], LcError> {
        hex_bytes(v)?.try_into().map_err(|_| LcError::Malformed(format!("expected {N} bytes")))
    }
    fn de<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T, LcError> {
        serde_json::from_value(v.clone()).map_err(|e| LcError::Malformed(e.to_string()))
    }

    pub fn committee(v: &Value) -> Result<SyncCommittee, LcError> {
        let pubkeys = v["pubkeys"].as_array().ok_or_else(|| LcError::Malformed("pubkeys".into()))?.iter().map(fixed::<48>).collect::<Result<Vec<_>, _>>()?;
        if pubkeys.len() != SYNC_COMMITTEE_SIZE {
            return Err(LcError::Malformed(format!("committee has {} keys", pubkeys.len())));
        }
        Ok(SyncCommittee { pubkeys, aggregate_pubkey: fixed::<48>(&v["aggregate_pubkey"])? })
    }

    pub fn finality_update(d: &Value) -> Result<FinalityUpdate, LcError> {
        Ok(FinalityUpdate {
            attested: de(&d["attested_header"]["beacon"])?,
            finalized: de(&d["finalized_header"]["beacon"])?,
            finality_branch: de(&d["finality_branch"])?,
            sync_committee_bits: hex_bytes(&d["sync_aggregate"]["sync_committee_bits"])?,
            sync_committee_signature: fixed::<96>(&d["sync_aggregate"]["sync_committee_signature"])?,
            signature_slot: d["signature_slot"].as_str().and_then(|s| s.parse().ok()).ok_or_else(|| LcError::Malformed("signature_slot".into()))?,
        })
    }

    pub fn fork_version_for(fork: &Value, signature_slot: u64) -> Result<[u8; 4], LcError> {
        let epoch = signature_slot.saturating_sub(1) / 32;
        let fork_epoch: u64 = fork["epoch"].as_str().and_then(|s| s.parse().ok()).ok_or_else(|| LcError::Malformed("fork.epoch".into()))?;
        fixed::<4>(if epoch >= fork_epoch { &fork["current_version"] } else { &fork["previous_version"] })
    }

    pub fn u256_dec(v: &Value) -> U256 {
        v.as_str().and_then(|s| U256::from_str_radix(s, 10).ok()).unwrap_or_default()
    }
}

pub fn verify_from_vectors(dir: &std::path::Path) -> Result<Store, LcError> {
    let read = |f: &str| -> Result<serde_json::Value, LcError> {
        let s = std::fs::read_to_string(dir.join(f)).map_err(|e| LcError::Malformed(format!("{f}: {e}")))?;
        serde_json::from_str(&s).map_err(|e| LcError::Malformed(format!("{f}: {e}")))
    };
    let de = |v: &serde_json::Value| -> Result<B256, LcError> { serde_json::from_value(v.clone()).map_err(|e| LcError::Malformed(e.to_string())) };
    let g = read("genesis.json")?;
    let meta = read("meta.json")?;
    let chain_id: u64 = meta["chain_id"].as_str().and_then(|s| s.parse().ok()).or(meta["chain_id"].as_u64()).ok_or_else(|| LcError::Malformed("chain_id".into()))?;
    let b = read("bootstrap.json")?;
    let header: BeaconBlockHeader = serde_json::from_value(b["data"]["header"]["beacon"].clone()).map_err(|e| LcError::Malformed(e.to_string()))?;
    let branch: Vec<B256> = serde_json::from_value(b["data"]["current_sync_committee_branch"].clone()).map_err(|e| LcError::Malformed(e.to_string()))?;
    let fu = read("finality_update.json")?;
    let forks = ForkSchedule {
        gloas_epoch: if fu["version"].as_str() == Some("gloas") { g["fork"]["epoch"].as_str().and_then(|s| s.parse().ok()) } else { None },
    };
    let mut store = bootstrap(
        chain_id,
        forks,
        de(&g["genesis"]["genesis_validators_root"])?,
        de(&b["checkpoint_root"])?,
        &header,
        json::committee(&b["data"]["current_sync_committee"])?,
        &branch,
    )?;
    let u = json::finality_update(&fu["data"])?;
    let fv = json::fork_version_for(&g["fork"], u.signature_slot)?;
    apply_finality_update(&mut store, &u, fv)?;
    Ok(store)
}
