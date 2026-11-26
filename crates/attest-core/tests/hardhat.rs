//! DESCROW deal on a local Hardhat node. Set HARDHAT_PROOFS to the proof dump to run.
use alloy_primitives::{B256, U256};
use attest_core::{loader::account_from_get_proof, *};

#[test]
fn descrow_deal_storage_proofs() {
    let Ok(path) = std::env::var("HARDHAT_PROOFS") else { eprintln!("SKIP"); return };
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let root: B256 = serde_json::from_value(v["state_root"].clone()).unwrap();
    let slot = |k: &str| -> B256 { serde_json::from_value(v["slots"][k].clone()).unwrap() };
    let (deal, deal_storage) = account_from_get_proof(&v["proof_deal"]).unwrap();
    let get = |k: B256| deal_storage.iter().find(|s| s.key == k).unwrap().clone();

    // escrowedAmount is 1000 minus the 2% platform fee
    let esc = get(slot("escrowedAmount"));
    let p = Predicate { subject: Subject::StorageSlot(esc.key), cmp: Cmp::Eq, rhs: U256::from(980_000_000u64) };
    verify_inclusion_and_predicate(root, &deal, Some(&esc), &p).expect("escrowedAmount proof");

    let st = get(slot("status"));
    let p = Predicate { subject: Subject::StorageSlot(st.key), cmp: Cmp::Eq, rhs: U256::from(1u64) };
    verify_inclusion_and_predicate(root, &deal, Some(&st), &p).expect("status proof");

    let (token, tstore) = account_from_get_proof(&v["proof_token"]).unwrap();
    let bal = tstore[0].clone();
    let p = Predicate { subject: Subject::StorageSlot(bal.key), cmp: Cmp::Gte, rhs: U256::from(980_000_000u64) };
    verify_inclusion_and_predicate(root, &token, Some(&bal), &p).expect("token balance proof");

    let p = Predicate { subject: Subject::StorageSlot(st.key), cmp: Cmp::Eq, rhs: U256::from(6u64) };
    assert_eq!(verify_inclusion_and_predicate(root, &deal, Some(&st), &p), Err(AttestError::PredicateFalse));
    let mut forged = esc.clone();
    forged.value = U256::from(1_000_000_000u64);
    let p = Predicate { subject: Subject::StorageSlot(esc.key), cmp: Cmp::Eq, rhs: forged.value };
    assert_eq!(verify_inclusion_and_predicate(root, &deal, Some(&forged), &p), Err(AttestError::StorageProofInvalid));
    println!("verified against EVM state root {root}");
}
