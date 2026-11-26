//! Runs against vectors/sepolia once fetch-proofs has been run.

use alloy_primitives::{B256, U256};
use attest_core::{loader::load_token_balance_claim, *};
use std::path::PathBuf;

fn dir() -> Option<PathBuf> {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vectors/sepolia");
    if d.join("meta.json").exists() { Some(d) } else { eprintln!("SKIP: no Sepolia vectors yet"); None }
}

#[test]
fn sepolia_descrow_balance_is_attested() {
    let Some(d) = dir() else { return };
    let l = load_token_balance_claim(&d, U256::from(1u64)).unwrap();
    let trusted = MemoryRoots { chain_id: l.claim.chain_id, roots: vec![l.finalized_beacon_root] };
    let st = attest(&l.claim, &trusted).expect("real Sepolia claim must verify");
    println!("statement: {st:#?}");
    let mut c = l.claim.clone();
    c.predicate.cmp = Cmp::Eq;
    c.predicate.rhs = l.expected_balance;
    attest(&c, &trusted).unwrap();
}

#[test]
fn sepolia_rejections() {
    let Some(d) = dir() else { return };
    let l = load_token_balance_claim(&d, U256::from(1u64)).unwrap();
    let trusted = MemoryRoots { chain_id: l.claim.chain_id, roots: vec![l.finalized_beacon_root] };

    // Not finalized on this instance
    let empty = MemoryRoots { chain_id: l.claim.chain_id, roots: vec![] };
    assert_eq!(attest(&l.claim, &empty), Err(AttestError::HeaderNotFinalized));
    // same contract address, different chain
    let mainnet = MemoryRoots { chain_id: 1, roots: trusted.roots.clone() };
    assert_eq!(attest(&l.claim, &mainnet), Err(AttestError::ChainIdMismatch));
    // Inflated balance
    let mut c = l.claim.clone();
    c.storage.as_mut().unwrap().value += U256::from(1u64);
    assert_eq!(attest(&c, &trusted), Err(AttestError::StorageProofInvalid));
    // Tampered committed execution data
    let mut c = l.claim.clone();
    match &mut c.execution {
        ExecutionAnchor::BlockHash { block_hash, .. } => *block_hash = B256::repeat_byte(1),
        ExecutionAnchor::PayloadHeader { execution, .. } => execution.state_root = B256::repeat_byte(1),
    }
    assert_eq!(attest(&c, &trusted), Err(AttestError::ExecutionBranchInvalid));
    // Predicate above the real balance
    let mut c = l.claim.clone();
    c.predicate.rhs = l.expected_balance + U256::from(1u64);
    assert_eq!(attest(&c, &trusted), Err(AttestError::PredicateFalse));
}

#[test]
fn sepolia_descrow_escrow_is_attested() {
    let Some(d) = dir() else { return };
    if !d.join("proof_deal.json").exists() { eprintln!("SKIP: no DESCROW deal proof yet"); return }
    let l = load_token_balance_claim(&d, U256::from(1u64)).unwrap();
    let trusted = MemoryRoots { chain_id: l.claim.chain_id, roots: vec![l.finalized_beacon_root] };
    for (label, c) in attest_core::loader::load_descrow_claims(&d, &l.claim).unwrap() {
        let st = attest(&c, &trusted).unwrap_or_else(|e| panic!("{label}: {e}"));
        println!("{label}: OK  id {}", st.id);
        let mut bad = c.clone();
        bad.predicate.rhs += U256::from(1u64);
        assert_eq!(attest(&bad, &trusted), Err(AttestError::PredicateFalse), "{label}");
    }
}

#[test]
fn sepolia_batch_matches_individual_attestations() {
    let Some(d) = dir() else { return };
    if !d.join("proof_deal.json").exists() { return }
    let l = load_token_balance_claim(&d, U256::from(1u64)).unwrap();
    let trusted = MemoryRoots { chain_id: l.claim.chain_id, roots: vec![l.finalized_beacon_root] };
    let mut claims = vec![l.claim.clone()];
    claims.extend(attest_core::loader::load_descrow_claims(&d, &l.claim).unwrap().into_iter().map(|(_, c)| c));
    let batch = attest_many(&claims, &trusted).unwrap();
    let single: Vec<_> = claims.iter().map(|c| attest(c, &trusted).unwrap()).collect();
    assert_eq!(batch, single, "batch and individual statements identical");
    // A bad claim anywhere fails the whole batch with its index
    let mut bad = claims.clone();
    bad[2].predicate.rhs += U256::from(1u64);
    assert_eq!(attest_many(&bad, &trusted).err(), Some((2, AttestError::PredicateFalse)));
    let empty = MemoryRoots { chain_id: l.claim.chain_id, roots: vec![] };
    assert_eq!(attest_many(&claims, &empty).err(), Some((0, AttestError::HeaderNotFinalized)));
}
