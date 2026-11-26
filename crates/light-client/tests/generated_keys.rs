//! Light client against a committee of 512 generated BLS keys.
use alloy_primitives::B256;
use attest_core::{ssz, BeaconBlockHeader};
use blst::min_pk::{AggregateSignature, SecretKey, Signature};
use light_client::*;

fn root_from_branch(leaf: [u8; 32], branch: &[B256], index: u64) -> B256 {
    let mut v = leaf;
    for (i, s) in branch.iter().enumerate() {
        v = if (index >> i) & 1 == 1 { ssz::sha256_pair(&s.0, &v) } else { ssz::sha256_pair(&v, &s.0) };
    }
    B256::from(v)
}

struct W { store: Store, update: FinalityUpdate, fv: [u8; 4], sks: Vec<SecretKey> }

fn world(signers: usize) -> W {
    let sks: Vec<SecretKey> = (0..512u32).map(|i| { let mut ikm = [7u8; 32]; ikm[..4].copy_from_slice(&i.to_le_bytes()); SecretKey::key_gen(&ikm, &[]).unwrap() }).collect();
    let pubkeys: Vec<[u8; 48]> = sks.iter().map(|k| k.sk_to_pk().to_bytes()).collect();
    let committee = SyncCommittee { pubkeys, aggregate_pubkey: [0u8; 48] };
    let gvr = B256::repeat_byte(0x42);
    let cbranch: Vec<B256> = (0..6).map(|i| B256::repeat_byte(0x30 + i)).collect();
    let boot = BeaconBlockHeader { slot: 8192 * 10, proposer_index: 1, parent_root: B256::ZERO, state_root: root_from_branch(committee.hash_tree_root(), &cbranch, 22), body_root: B256::ZERO };
    let store = bootstrap(11155111, ForkSchedule { gloas_epoch: None }, gvr, boot.hash_tree_root(), &boot, committee, &cbranch).unwrap();
    let finalized = BeaconBlockHeader { slot: 8192 * 10 + 64, proposer_index: 2, parent_root: B256::ZERO, state_root: B256::repeat_byte(5), body_root: B256::repeat_byte(6) };
    let fbranch: Vec<B256> = (0..7).map(|i| B256::repeat_byte(0x50 + i)).collect();
    let attested = BeaconBlockHeader { slot: 8192 * 10 + 128, proposer_index: 3, parent_root: B256::ZERO, state_root: root_from_branch(finalized.hash_tree_root().0, &fbranch, 41), body_root: B256::ZERO };
    let fv = [0x90, 0, 0, 0x69];
    let msg = signing_root(attested.hash_tree_root(), compute_domain(DOMAIN_SYNC_COMMITTEE, fv, gvr));
    let sigs: Vec<Signature> = sks[..signers].iter().map(|k| k.sign(&msg, BLS_DST, &[])).collect();
    let agg = AggregateSignature::aggregate(&sigs.iter().collect::<Vec<_>>(), true).unwrap().to_signature();
    let mut bits = vec![0u8; 64];
    for i in 0..signers { bits[i / 8] |= 1 << (i % 8); }
    let update = FinalityUpdate { attested, finalized, finality_branch: fbranch, sync_committee_bits: bits, sync_committee_signature: agg.to_bytes(), signature_slot: 8192 * 10 + 129 };
    W { store, update, fv, sks }
}

#[test]
fn supermajority_signed_update_is_accepted() {
    let mut w = world(400);
    let r = apply_finality_update(&mut w.store, &w.update, w.fv).unwrap();
    assert_eq!(r, w.update.finalized.hash_tree_root());
    assert!(w.store.finalized_roots.contains(&r));
}

#[test]
fn threshold_is_exactly_two_thirds() {
    let mut w = world(342);
    assert!(apply_finality_update(&mut w.store, &w.update, w.fv).is_ok(), "342/512 passes");
    let mut w = world(341);
    assert_eq!(apply_finality_update(&mut w.store, &w.update, w.fv), Err(LcError::InsufficientParticipation(341)));
}

#[test]
fn forged_signatures_and_branches_are_rejected() {
    let w = world(400);
    let t = |u: &FinalityUpdate, fv| apply_finality_update(&mut w.store.clone(), u, fv);
    // signer bits don't match the signature
    let mut u = clone(&w.update); u.sync_committee_bits[50] |= 1; assert_eq!(t(&u, w.fv), Err(LcError::SignatureInvalid));
    // signed by keys outside the committee
    let mut u = clone(&w.update);
    let other: Vec<Signature> = (0..400u32).map(|i| { let mut ikm = [9u8; 32]; ikm[..4].copy_from_slice(&i.to_le_bytes()); SecretKey::key_gen(&ikm, &[]).unwrap().sign(b"x", BLS_DST, &[]) }).collect();
    u.sync_committee_signature = AggregateSignature::aggregate(&other.iter().collect::<Vec<_>>(), true).unwrap().to_signature().to_bytes();
    assert_eq!(t(&u, w.fv), Err(LcError::SignatureInvalid));
    // Wrong fork version
    assert_eq!(t(&w.update, [1, 2, 3, 4]), Err(LcError::SignatureInvalid));
    // Finalized header not in attested state
    let mut u = clone(&w.update); u.finalized.slot += 1; assert_eq!(t(&u, w.fv), Err(LcError::FinalityBranchInvalid));
    // Signature slot in another period
    let mut u = clone(&w.update); u.signature_slot += 8192; assert_eq!(t(&u, w.fv), Err(LcError::WrongPeriod));
    let _ = &w.sks;
}

fn clone(u: &FinalityUpdate) -> FinalityUpdate {
    FinalityUpdate { attested: u.attested.clone(), finalized: u.finalized.clone(), finality_branch: u.finality_branch.clone(), sync_committee_bits: u.sync_committee_bits.clone(), sync_committee_signature: u.sync_committee_signature, signature_slot: u.signature_slot }
}
