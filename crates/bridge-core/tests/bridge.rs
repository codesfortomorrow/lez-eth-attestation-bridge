//! hardhat_*: local vault and proofs under a synthetic beacon anchor.
//! sepolia_*: live deposits under the BLS-verified anchor.
use alloy_primitives::{Address, Bytes, B256, U256};
use attest_core::{loader::account_from_get_proof, ssz, *};
use bridge_core::*;
use serde_json::Value;

struct Fixture { cfg: BridgeConfig, trusted: MemoryRoots, reqs: Vec<MintRequest>, vault_eth: U256, vault_usdt: U256, usdt: Address }

fn v(x: &Value) -> String { x.as_str().unwrap().to_string() }
fn b256(x: &Value) -> B256 { v(x).parse().unwrap() }

fn synthetic_anchor(chain_id: u64, block_hash: B256) -> (BeaconBlockHeader, ExecutionAnchor, MemoryRoots) {
    let branch: Vec<B256> = (0..11u8).map(|i| B256::repeat_byte(0x40 + i)).collect();
    let mut acc = block_hash.0;
    let idx = ssz::EXECUTION_BLOCK_HASH_GINDEX_GLOAS - 2048;
    for (i, s) in branch.iter().enumerate() {
        acc = if (idx >> i) & 1 == 1 { ssz::sha256_pair(&s.0, &acc) } else { ssz::sha256_pair(&acc, &s.0) };
    }
    let bh = BeaconBlockHeader { slot: 353_024 * 32 + 5, proposer_index: 7, parent_root: B256::ZERO, state_root: B256::repeat_byte(9), body_root: B256::from(acc) };
    let trusted = MemoryRoots { chain_id, roots: vec![bh.hash_tree_root()] };
    (bh, ExecutionAnchor::BlockHash { block_hash, branch }, trusted)
}

fn claim_for(base: &Claim, account: &AccountProof, sp: &StorageProof) -> Claim {
    let mut c = base.clone();
    c.account = account.clone();
    c.storage = Some(sp.clone());
    c.predicate = Predicate { subject: Subject::StorageSlot(sp.key), cmp: Cmp::Eq, rhs: sp.value };
    c
}

fn build(bridge: &Value, base: &Claim, trusted: MemoryRoots, proof_vault: &Value) -> Fixture {
    let (vault_acct, storage) = account_from_get_proof(proof_vault).unwrap();
    let usdt: Address = v(&bridge["usdt"]).parse().unwrap();
    let cfg = BridgeConfig {
        eth_chain_id: base.chain_id,
        vault: v(&bridge["vault"]).parse().unwrap(),
        lez_chain_id: u64::from_str_radix(v(&bridge["lez_chain_id"]).trim_start_matches("0x"), if v(&bridge["lez_chain_id"]).starts_with("0x") { 16 } else { 10 }).unwrap(),
        registry: vec![(usdt, B256::repeat_byte(0xa1)), (Address::ZERO, B256::repeat_byte(0xe7))],
    };
    let mut reqs = vec![];
    let (mut vault_eth, mut vault_usdt) = (U256::ZERO, U256::ZERO);
    for d in bridge["deposits"].as_array().unwrap() {
        let id = d["id"].as_u64().unwrap();
        let (s0, s1) = deposit_slots(id);
        let get = |k: B256| storage.iter().find(|s| s.key == k).unwrap().clone();
        reqs.push(MintRequest {
            deposit_id: id,
            commitment_claim: claim_for(base, &vault_acct, &get(s0)),
            record_claim: claim_for(base, &vault_acct, &get(s1)),
            secret: b256(&d["test_only_secret"]),
            recipient: b256(&d["test_only_recipient"]),
        });
        let amt = U256::from_str_radix(&v(&d["amount"]), 10).unwrap();
        if v(&d["token"]).parse::<Address>().unwrap() == Address::ZERO { vault_eth += amt } else { vault_usdt += amt }
    }
    Fixture { cfg, trusted, reqs, vault_eth, vault_usdt, usdt }
}

fn local() -> Option<Fixture> {
    let path = std::env::var("HARDHAT_BRIDGE").ok()?;
    let j: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let chain_id = 31337u64;
    let (bh, anchor, trusted) = synthetic_anchor(chain_id, b256(&j["block_hash"]));
    let base = Claim {
        chain_id, beacon_header: bh, execution: anchor,
        exec_header_rlp: v(&j["header_rlp"]).parse::<Bytes>().unwrap(),
        account: AccountProof { address: Address::ZERO, nonce: 0, balance: U256::ZERO, storage_hash: B256::ZERO, code_hash: B256::ZERO, proof: vec![] },
        storage: None,
        predicate: Predicate { subject: Subject::AccountBalance, cmp: Cmp::Gte, rhs: U256::ZERO },
    };
    Some(build(&j["bridge"], &base, trusted, &j["proof_vault"]))
}

fn sepolia() -> Option<Fixture> {
    let d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vectors/sepolia");
    if !d.join("proof_vault.json").exists() { eprintln!("SKIP: no Sepolia vault vectors"); return None; }
    let read = |f: &str| -> Value { serde_json::from_str(&std::fs::read_to_string(d.join(f)).unwrap()).unwrap() };
    let l = attest_core::loader::load_token_balance_claim(&d, U256::from(1u64)).unwrap();
    let store = light_client::verify_from_vectors(&d).expect("BLS light-client verification");
    assert!(store.finalized_roots.contains(&l.finalized_beacon_root));
    let trusted = MemoryRoots { chain_id: store.chain_id, roots: store.finalized_roots.clone() };
    Some(build(&read("bridge.json"), &l.claim, trusted, &read("proof_vault.json")))
}

fn run_all_checks(f: Fixture, label: &str) {
    let mut state = BridgeState::default();
    for r in &f.reqs {
        let m = verify_and_mint(&f.cfg, r, &f.trusted, &mut state).unwrap_or_else(|e| panic!("{label} mint #{}: {e}", r.deposit_id));
        println!("{label}: minted {} of {:?} to {} (deposit #{}, nullifier {})", m.amount, m.wrapped_mint, m.recipient, m.deposit_id, m.nullifier);
    }
    // wrapped supply must equal what the vault holds
    let sup = |m: B256| state.supply.iter().find(|(x, _)| *x == m).map(|(_, s)| *s).unwrap_or_default();
    assert_eq!(sup(B256::repeat_byte(0xa1)), f.vault_usdt);
    assert_eq!(sup(B256::repeat_byte(0xe7)), f.vault_eth);
    let _ = f.usdt;

    let r = &f.reqs[0];
    // double mint
    assert_eq!(verify_and_mint(&f.cfg, r, &f.trusted, &mut state), Err(BridgeError::AlreadyMinted));
    let mut fresh = BridgeState::default();
    // relayer redirects to itself
    let mut x = r.clone(); x.recipient = B256::repeat_byte(0x66);
    assert_eq!(verify_and_mint(&f.cfg, &x, &f.trusted, &mut fresh), Err(BridgeError::BadOpening));
    // wrong secret
    let mut x = r.clone(); x.secret = B256::repeat_byte(1);
    assert_eq!(verify_and_mint(&f.cfg, &x, &f.trusted, &mut fresh), Err(BridgeError::BadOpening));
    // deposit #1's opening on deposit #0
    let mut x = r.clone(); x.secret = f.reqs[1].secret; x.recipient = f.reqs[1].recipient;
    assert_eq!(verify_and_mint(&f.cfg, &x, &f.trusted, &mut fresh), Err(BridgeError::BadOpening));
    // wrong deposit id
    let mut x = r.clone(); x.deposit_id = 99;
    assert_eq!(verify_and_mint(&f.cfg, &x, &f.trusted, &mut fresh), Err(BridgeError::WrongSlots));
    // different vault
    let mut cfg = f.cfg.clone(); cfg.vault = Address::repeat_byte(0x11);
    assert_eq!(verify_and_mint(&cfg, r, &f.trusted, &mut fresh), Err(BridgeError::WrongVault));
    // inflated amount
    let mut x = r.clone(); let sp = x.record_claim.storage.as_mut().unwrap(); sp.value += U256::from(1u64) << 160; x.record_claim.predicate.rhs = sp.value;
    assert!(matches!(verify_and_mint(&f.cfg, &x, &f.trusted, &mut fresh), Err(BridgeError::Attestation(AttestError::StorageProofInvalid))));
    // not finalized
    let empty = MemoryRoots { chain_id: f.trusted.chain_id, roots: vec![] };
    assert!(matches!(verify_and_mint(&f.cfg, r, &empty, &mut fresh), Err(BridgeError::Attestation(AttestError::HeaderNotFinalized))));
    // unregistered token
    let mut cfg = f.cfg.clone(); cfg.registry.clear();
    assert_eq!(verify_and_mint(&cfg, r, &f.trusted, &mut fresh), Err(BridgeError::UnsupportedToken));
    assert!(fresh.spent.is_empty(), "rejected claims consume nothing");
}

#[test]
fn hardhat_vault_deposits_mint_once_each() { let Some(f) = local() else { eprintln!("SKIP"); return }; run_all_checks(f, "hardhat"); }

#[test]
fn sepolia_vault_deposits_mint_once_each() { let Some(f) = sepolia() else { return }; run_all_checks(f, "sepolia"); }
