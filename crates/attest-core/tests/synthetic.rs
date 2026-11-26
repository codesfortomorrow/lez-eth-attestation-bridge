//! Offline tests on state we build ourselves, so every rejection path can be hit.

use alloy_primitives::{address, b256, keccak256, Address, Bytes, B256, U256};
use alloy_rlp::Encodable;
use alloy_trie::{proof::ProofRetainer, HashBuilder, Nibbles};
use attest_core::*;

const SEPOLIA: u64 = 11_155_111;
const TOKEN: Address = address!("00000000000000000000000000000000000000aa");
const HOLDER: Address = address!("00000000000000000000000000000000000000bb");
const OTHER: Address = address!("00000000000000000000000000000000000000cc");

fn trie_with_proof(mut leaves: Vec<(B256, Vec<u8>)>, target: B256) -> (B256, Vec<Bytes>) {
    leaves.sort_by_key(|(k, _)| *k);
    let t = Nibbles::unpack(target);
    let mut hb = HashBuilder::default().with_proof_retainer(ProofRetainer::new(vec![t]));
    for (k, v) in &leaves {
        hb.add_leaf(Nibbles::unpack(*k), v);
    }
    let root = hb.root();
    let proof = hb.take_proof_nodes().matching_nodes_sorted(&t).into_iter().map(|(_, b)| b).collect();
    (root, proof)
}

fn rlp<T: Encodable>(v: T) -> Vec<u8> {
    let mut o = Vec::new();
    v.encode(&mut o);
    o
}

fn account_rlp(nonce: u64, bal: U256, sroot: B256, code: B256) -> Vec<u8> {
    let mut p = Vec::new();
    nonce.encode(&mut p);
    bal.encode(&mut p);
    sroot.encode(&mut p);
    code.encode(&mut p);
    let mut o = Vec::new();
    alloy_rlp::Header { list: true, payload_length: p.len() }.encode(&mut o);
    o.extend(p);
    o
}

fn balance_slot(holder: Address, mapping_slot: u64) -> B256 {
    let mut buf = [0u8; 64];
    buf[12..32].copy_from_slice(holder.as_slice());
    buf[56..64].copy_from_slice(&mapping_slot.to_be_bytes());
    keccak256(buf)
}

/// Minimal 15-field header RLP with a chosen state root.
fn header_rlp(state_root: B256, number: u64) -> Bytes {
    let z = B256::ZERO;
    let mut p = Vec::new();
    z.encode(&mut p); // parentHash
    b256!("1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347").encode(&mut p); // ommers
    Address::ZERO.encode(&mut p); // beneficiary
    state_root.encode(&mut p);
    z.encode(&mut p); // txRoot
    z.encode(&mut p); // receiptsRoot
    Bytes::from(vec![0u8; 256]).encode(&mut p); // bloom
    U256::ZERO.encode(&mut p); // difficulty
    number.encode(&mut p);
    30_000_000u64.encode(&mut p);
    0u64.encode(&mut p);
    1_700_000_000u64.encode(&mut p);
    Bytes::new().encode(&mut p);
    z.encode(&mut p); // mixHash
    alloy_primitives::FixedBytes::<8>::ZERO.encode(&mut p); // nonce
    let mut o = Vec::new();
    alloy_rlp::Header { list: true, payload_length: p.len() }.encode(&mut o);
    o.extend(p);
    o.into()
}

struct World {
    claim: Claim,
    trusted: MemoryRoots,
    state_root: B256,
}

fn build_world(token_balance_of_holder: u64, number: u64, seed: u8) -> World {
    build_world_fork(token_balance_of_holder, number, seed, true)
}

/// gloas = true: Sepolia layout (block hash, gindex 2856); false: payload header (gindex 25).
fn build_world_fork(token_balance_of_holder: u64, number: u64, seed: u8, gloas: bool) -> World {
    let slot_h = balance_slot(HOLDER, 0);
    let slot_o = balance_slot(OTHER, 0);
    let (sroot, sproof) = trie_with_proof(
        vec![
            (keccak256(slot_h), rlp(U256::from(token_balance_of_holder))),
            (keccak256(slot_o), rlp(U256::from(5u64))),
        ],
        keccak256(slot_h),
    );
    let code_hash = keccak256(b"token code");
    let (state_root, aproof) = trie_with_proof(
        vec![
            (keccak256(TOKEN), account_rlp(1, U256::ZERO, sroot, code_hash)),
            (keccak256(HOLDER), account_rlp(0, U256::from(7u64), alloy_trie::EMPTY_ROOT_HASH, keccak256(b"deal"))),
        ],
        keccak256(TOKEN),
    );
    let exec_rlp = header_rlp(state_root, number);
    let block_hash = keccak256(&exec_rlp);
    let execution = ExecutionPayloadHeader {
        parent_hash: B256::ZERO,
        fee_recipient: Address::ZERO,
        state_root,
        receipts_root: B256::ZERO,
        logs_bloom: vec![0u8; 256].into(),
        prev_randao: B256::repeat_byte(seed),
        block_number: number,
        gas_limit: 30_000_000,
        gas_used: 0,
        timestamp: 1_700_000_000,
        extra_data: Bytes::new(),
        base_fee_per_gas: U256::from(7u64),
        block_hash,
        transactions_root: B256::ZERO,
        withdrawals_root: B256::ZERO,
        blob_gas_used: 0,
        excess_blob_gas: 0,
    };
    let (leaf, depth, index) = if gloas { (block_hash.0, 11usize, 808u64) } else { (execution.hash_tree_root(), 4, 9) };
    let branch: Vec<B256> = (0..depth as u8).map(|i| B256::repeat_byte(0x10 + i + seed)).collect();
    let mut v = leaf;
    for (i, s) in branch.iter().enumerate() {
        v = if (index >> i) & 1 == 1 { ssz::sha256_pair(&s.0, &v) } else { ssz::sha256_pair(&v, &s.0) };
    }
    let beacon_header = BeaconBlockHeader {
        slot: 1000 + number,
        proposer_index: 42,
        parent_root: B256::ZERO,
        state_root: B256::repeat_byte(seed),
        body_root: B256::from(v),
    };
    let trusted = MemoryRoots { chain_id: SEPOLIA, roots: vec![beacon_header.hash_tree_root()] };
    let claim = Claim {
        chain_id: SEPOLIA,
        beacon_header,
        execution: if gloas {
            ExecutionAnchor::BlockHash { block_hash, branch }
        } else {
            ExecutionAnchor::PayloadHeader { execution, branch }
        },
        exec_header_rlp: exec_rlp,
        account: AccountProof {
            address: TOKEN,
            nonce: 1,
            balance: U256::ZERO,
            storage_hash: sroot,
            code_hash,
            proof: aproof,
        },
        storage: Some(StorageProof { key: slot_h, value: U256::from(token_balance_of_holder), proof: sproof }),
        predicate: Predicate { subject: Subject::StorageSlot(slot_h), cmp: Cmp::Gte, rhs: U256::from(1000u64) },
    };
    World { claim, trusted, state_root }
}

#[test]
fn honest_claim_is_attested() {
    let w = build_world(1_500, 100, 1);
    let st = attest(&w.claim, &w.trusted).expect("valid");
    assert_eq!(st.chain_id, SEPOLIA);
    assert_eq!(st.block_number, 100);
    assert_eq!(st.account, TOKEN);
}

#[test]
fn predicate_false_is_rejected() {
    let w = build_world(999, 100, 1);
    assert_eq!(attest(&w.claim, &w.trusted), Err(AttestError::PredicateFalse));
}

#[test]
fn all_comparison_operators() {
    let w = build_world(1_000, 100, 1);
    for (cmp, rhs, ok) in [
        (Cmp::Eq, 1000u64, true),
        (Cmp::Ne, 1000, false),
        (Cmp::Gt, 999, true),
        (Cmp::Gte, 1001, false),
        (Cmp::Lt, 1001, true),
        (Cmp::Lte, 999, false),
    ] {
        let mut c = w.claim.clone();
        c.predicate.cmp = cmp;
        c.predicate.rhs = U256::from(rhs);
        assert_eq!(attest(&c, &w.trusted).is_ok(), ok, "{cmp:?} {rhs}");
    }
}

/// A fully self-consistent fake chain passes the inclusion check but not attestation,
/// because its beacon root was never finalized on the instance.
#[test]
fn fabricated_chain_is_rejected() {
    let real = build_world(1_500, 100, 1);
    let fake = build_world(1_000_000, 100, 9);

    assert!(verify_inclusion_and_predicate(
        fake.state_root,
        &fake.claim.account,
        fake.claim.storage.as_ref(),
        &fake.claim.predicate
    )
    .is_ok());

    assert_eq!(attest(&fake.claim, &real.trusted), Err(AttestError::HeaderNotFinalized));
}

#[test]
fn tampered_storage_value_is_rejected() {
    let mut w = build_world(1_500, 100, 1);
    w.claim.storage.as_mut().unwrap().value = U256::from(2_000_000u64);
    assert_eq!(attest(&w.claim, &w.trusted), Err(AttestError::StorageProofInvalid));
}

#[test]
fn tampered_storage_proof_node_is_rejected() {
    let mut w = build_world(1_500, 100, 1);
    let sp = w.claim.storage.as_mut().unwrap();
    let mut node = sp.proof[0].to_vec();
    let last = node.len() - 1;
    node[last] ^= 1;
    sp.proof[0] = node.into();
    assert_eq!(attest(&w.claim, &w.trusted), Err(AttestError::StorageProofInvalid));
}

#[test]
fn tampered_account_storage_root_is_rejected() {
    let mut w = build_world(1_500, 100, 1);
    w.claim.account.storage_hash = B256::repeat_byte(7);
    assert_eq!(attest(&w.claim, &w.trusted), Err(AttestError::AccountProofInvalid));
}

#[test]
fn tampered_committed_block_hash_is_rejected() {
    let mut w = build_world(1_500, 100, 1);
    if let ExecutionAnchor::BlockHash { block_hash, .. } = &mut w.claim.execution {
        *block_hash = B256::repeat_byte(3);
    }
    assert_eq!(attest(&w.claim, &w.trusted), Err(AttestError::ExecutionBranchInvalid));
}

#[test]
fn pre_gloas_layout_honest_and_tampered() {
    let w = build_world_fork(1_500, 100, 1, false);
    attest(&w.claim, &w.trusted).expect("Capella-Fulu layout verifies");
    let mut c = w.claim.clone();
    if let ExecutionAnchor::PayloadHeader { execution, .. } = &mut c.execution {
        execution.state_root = B256::repeat_byte(3);
    }
    assert_eq!(attest(&c, &w.trusted), Err(AttestError::ExecutionBranchInvalid));
    let fake = build_world_fork(1_000_000, 100, 9, false);
    assert_eq!(attest(&fake.claim, &w.trusted), Err(AttestError::HeaderNotFinalized));
}

#[test]
fn swapped_exec_header_is_rejected() {
    let w = build_world(1_500, 100, 1);
    let other = build_world(1_500, 101, 1);
    let mut c = w.claim.clone();
    c.exec_header_rlp = other.claim.exec_header_rlp.clone();
    assert_eq!(attest(&c, &w.trusted), Err(AttestError::ExecHeaderMismatch));
}

#[test]
fn wrong_chain_id_is_rejected() {
    let w = build_world(1_500, 100, 1);
    let mainnet_instance = MemoryRoots { chain_id: 1, roots: w.trusted.roots.clone() };
    assert_eq!(attest(&w.claim, &mainnet_instance), Err(AttestError::ChainIdMismatch));
    let mut c = w.claim.clone();
    c.chain_id = 1;
    assert_eq!(attest(&c, &w.trusted), Err(AttestError::ChainIdMismatch));
}

#[test]
fn predicate_over_uncovered_slot_is_rejected() {
    let mut w = build_world(1_500, 100, 1);
    w.claim.predicate.subject = Subject::StorageSlot(balance_slot(OTHER, 0));
    assert!(matches!(attest(&w.claim, &w.trusted), Err(AttestError::Malformed(_))));
}

#[test]
fn same_fact_at_different_blocks_has_same_id() {
    let a = build_world(1_500, 100, 1);
    let b = build_world(1_500, 200, 2);
    let sa = attest(&a.claim, &a.trusted).unwrap();
    let sb = attest(&b.claim, &b.trusted).unwrap();
    assert_ne!(sa.block_hash, sb.block_hash);
    assert_eq!(sa.id, sb.id);
}

#[test]
fn statement_id_is_deterministic_and_distinct() {
    let a = attest(&build_world(1_500, 100, 1).claim, &build_world(1_500, 100, 1).trusted).unwrap();
    let b = attest(&build_world(1_500, 100, 1).claim, &build_world(1_500, 100, 1).trusted).unwrap();
    assert_eq!(a, b, "same inputs, same statement and id");
    let c = attest(&build_world(1_501, 100, 1).claim, &build_world(1_501, 100, 1).trusted).unwrap();
    assert_ne!(a.id, c.id, "different attested state, different id");
}

#[test]
fn mainnet_genesis_header_decodes_to_known_hash() {
    let rlp = hex::decode(include_str!("fixtures/mainnet_genesis_header.hex").trim()).unwrap();
    let h = ExecHeader::decode(&rlp).unwrap();
    assert_eq!(h.hash, b256!("d4e56740f876aef8c010b86a40d5f56745a118d0906a34e69aec8c0db1cb8fa3"));
    assert_eq!(h.number, 0);
    assert_eq!(h.state_root, b256!("d7f8974fb5ac78d9ac099b9ad5018bedc2ce0a72dad1827a1709da30580f0544"));
}
