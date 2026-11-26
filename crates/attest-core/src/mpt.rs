//! Account and storage proofs, checked with alloy-trie.

use crate::AttestError;
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_rlp::Encodable;
use alloy_trie::{proof::verify_proof, Nibbles, EMPTY_ROOT_HASH, KECCAK_EMPTY};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountProof {
    pub address: Address,
    pub nonce: u64,
    pub balance: U256,
    pub storage_hash: B256,
    pub code_hash: B256,
    pub proof: Vec<Bytes>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageProof {
    pub key: B256,
    pub value: U256,
    pub proof: Vec<Bytes>,
}

fn account_rlp(a: &AccountProof) -> Vec<u8> {
    let mut payload = Vec::new();
    a.nonce.encode(&mut payload);
    a.balance.encode(&mut payload);
    a.storage_hash.encode(&mut payload);
    a.code_hash.encode(&mut payload);
    let mut out = Vec::with_capacity(payload.len() + 3);
    alloy_rlp::Header { list: true, payload_length: payload.len() }.encode(&mut out);
    out.extend_from_slice(&payload);
    out
}

fn is_empty_account(a: &AccountProof) -> bool {
    a.nonce == 0 && a.balance.is_zero() && a.storage_hash == EMPTY_ROOT_HASH && a.code_hash == KECCAK_EMPTY
}

/// An absent account is proven by exclusion and must be claimed as empty.
pub fn verify_account(state_root: B256, a: &AccountProof) -> Result<(), AttestError> {
    let key = Nibbles::unpack(keccak256(a.address));
    let expected = if is_empty_account(a) { None } else { Some(account_rlp(a)) };
    verify_proof(state_root, key, expected, a.proof.iter()).map_err(|_| AttestError::AccountProofInvalid)
}

/// Zero slots are not stored in the trie, so a zero value is proven by exclusion.
pub fn verify_storage(storage_root: B256, s: &StorageProof) -> Result<(), AttestError> {
    let key = Nibbles::unpack(keccak256(s.key));
    let expected = if s.value.is_zero() {
        None
    } else {
        let mut v = Vec::new();
        s.value.encode(&mut v);
        Some(v)
    };
    verify_proof(storage_root, key, expected, s.proof.iter()).map_err(|_| AttestError::StorageProofInvalid)
}
