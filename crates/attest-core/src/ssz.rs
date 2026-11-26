//! SSZ hashing for the consensus containers we need, and Merkle branch checks.

use alloy_primitives::{Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub fn sha256_pair(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(a);
    h.update(b);
    h.finalize().into()
}

pub fn merkleize(chunks: &[[u8; 32]], limit: Option<usize>) -> [u8; 32] {
    let n = limit.unwrap_or(chunks.len()).max(1).next_power_of_two();
    let mut layer: Vec<[u8; 32]> = chunks.to_vec();
    layer.resize(n, [0u8; 32]);
    while layer.len() > 1 {
        layer = layer.chunks(2).map(|p| sha256_pair(&p[0], &p[1])).collect();
    }
    layer[0]
}

fn u64_leaf(v: u64) -> [u8; 32] {
    let mut c = [0u8; 32];
    c[..8].copy_from_slice(&v.to_le_bytes());
    c
}

fn u256_leaf(v: U256) -> [u8; 32] {
    v.to_le_bytes::<32>()
}

fn bytes_chunks(b: &[u8]) -> Vec<[u8; 32]> {
    b.chunks(32)
        .map(|c| {
            let mut x = [0u8; 32];
            x[..c.len()].copy_from_slice(c);
            x
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeaconBlockHeader {
    #[serde(with = "dec_u64")]
    pub slot: u64,
    #[serde(with = "dec_u64")]
    pub proposer_index: u64,
    pub parent_root: B256,
    pub state_root: B256,
    pub body_root: B256,
}

impl BeaconBlockHeader {
    pub fn hash_tree_root(&self) -> B256 {
        B256::from(merkleize(
            &[u64_leaf(self.slot), u64_leaf(self.proposer_index), self.parent_root.0, self.state_root.0, self.body_root.0],
            None,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPayloadHeader {
    pub parent_hash: B256,
    pub fee_recipient: Address,
    pub state_root: B256,
    pub receipts_root: B256,
    pub logs_bloom: Bytes,
    pub prev_randao: B256,
    #[serde(with = "dec_u64")]
    pub block_number: u64,
    #[serde(with = "dec_u64")]
    pub gas_limit: u64,
    #[serde(with = "dec_u64")]
    pub gas_used: u64,
    #[serde(with = "dec_u64")]
    pub timestamp: u64,
    pub extra_data: Bytes,
    #[serde(with = "dec_u256")]
    pub base_fee_per_gas: U256,
    pub block_hash: B256,
    pub transactions_root: B256,
    pub withdrawals_root: B256,
    #[serde(with = "dec_u64")]
    pub blob_gas_used: u64,
    #[serde(with = "dec_u64")]
    pub excess_blob_gas: u64,
}

impl ExecutionPayloadHeader {
    pub fn hash_tree_root(&self) -> [u8; 32] {
        let mut fee = [0u8; 32];
        fee[..20].copy_from_slice(self.fee_recipient.as_slice());
        let bloom = merkleize(&bytes_chunks(&self.logs_bloom), Some(8));
        let mut len = [0u8; 32];
        len[..8].copy_from_slice(&(self.extra_data.len() as u64).to_le_bytes());
        let extra = sha256_pair(&merkleize(&bytes_chunks(&self.extra_data), Some(1)), &len);
        merkleize(
            &[
                self.parent_hash.0,
                fee,
                self.state_root.0,
                self.receipts_root.0,
                bloom,
                self.prev_randao.0,
                u64_leaf(self.block_number),
                u64_leaf(self.gas_limit),
                u64_leaf(self.gas_used),
                u64_leaf(self.timestamp),
                extra,
                u256_leaf(self.base_fee_per_gas),
                self.block_hash.0,
                self.transactions_root.0,
                self.withdrawals_root.0,
                u64_leaf(self.blob_gas_used),
                u64_leaf(self.excess_blob_gas),
            ],
            None,
        )
    }
}

pub fn is_valid_merkle_branch(leaf: [u8; 32], branch: &[B256], depth: usize, index: u64, root: B256) -> bool {
    if branch.len() != depth {
        return false;
    }
    let mut value = leaf;
    for (i, sib) in branch.iter().enumerate() {
        value = if (index >> i) & 1 == 1 { sha256_pair(&sib.0, &value) } else { sha256_pair(&value, &sib.0) };
    }
    value == root.0
}

pub const EXECUTION_BRANCH_DEPTH: usize = 4;
pub const EXECUTION_PAYLOAD_INDEX: u64 = 9;

pub fn verify_execution_branch(exec: &ExecutionPayloadHeader, branch: &[B256], body_root: B256) -> bool {
    is_valid_merkle_branch(exec.hash_tree_root(), branch, EXECUTION_BRANCH_DEPTH, EXECUTION_PAYLOAD_INDEX, body_root)
}

pub fn is_valid_gindex_branch(leaf: [u8; 32], branch: &[B256], gindex: u64, root: B256) -> bool {
    let depth = 63 - gindex.leading_zeros() as usize;
    is_valid_merkle_branch(leaf, branch, depth, gindex - (1u64 << depth), root)
}

/// Gloas with EIP-7688 progressive containers (Sepolia since epoch 353024).
/// Taken from live data: the only position that verifies. See README, D-004.
pub const EXECUTION_BLOCK_HASH_GINDEX_GLOAS: u64 = 2856;

pub fn verify_gloas_block_hash_branch(block_hash: B256, branch: &[B256], body_root: B256) -> bool {
    is_valid_gindex_branch(block_hash.0, branch, EXECUTION_BLOCK_HASH_GINDEX_GLOAS, body_root)
}

// Beacon API sends integers as decimal strings.
mod dec_u64 {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        String::deserialize(d)?.parse().map_err(D::Error::custom)
    }
}

mod dec_u256 {
    use alloy_primitives::U256;
    use serde::{de::Error, Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &U256, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<U256, D::Error> {
        U256::from_str_radix(&String::deserialize(d)?, 10).map_err(D::Error::custom)
    }
}
