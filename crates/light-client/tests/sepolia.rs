//! Full path on live Sepolia data. Nothing goes into the trusted set by hand.
use alloy_primitives::{B256, U256};
use attest_core::{attest, loader, AttestError};
use light_client::{json, *};
use serde_json::Value;
use std::path::PathBuf;

fn dir() -> Option<PathBuf> {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vectors/sepolia");
    if d.join("bootstrap.json").exists() && d.join("finality_update.json").exists() { Some(d) } else { eprintln!("SKIP: no Sepolia light-client vectors"); None }
}
fn read(d: &PathBuf, f: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(d.join(f)).unwrap()).unwrap()
}

struct Ctx {
    store: Store,
    update: FinalityUpdate,
    fork_version: [u8; 4],
}

fn setup(d: &PathBuf) -> Ctx {
    let store = verify_bootstrap_only(d);
    let g = read(d, "genesis.json");
    let update = json::finality_update(&read(d, "finality_update.json")["data"]).unwrap();
    let fork_version = json::fork_version_for(&g["fork"], update.signature_slot).unwrap();
    Ctx { store, update, fork_version }
}

fn forks(d: &PathBuf) -> ForkSchedule {
    ForkSchedule { gloas_epoch: read(d, "genesis.json")["fork"]["epoch"].as_str().unwrap().parse().ok() }
}

fn verify_bootstrap_only(d: &PathBuf) -> Store {
    let g = read(d, "genesis.json");
    let gvr: B256 = serde_json::from_value(g["genesis"]["genesis_validators_root"].clone()).unwrap();
    let chain_id: u64 = read(d, "meta.json")["chain_id"].as_str().unwrap().parse().unwrap();
    let b = read(d, "bootstrap.json");
    let checkpoint: B256 = serde_json::from_value(b["checkpoint_root"].clone()).unwrap();
    let header: attest_core::BeaconBlockHeader = serde_json::from_value(b["data"]["header"]["beacon"].clone()).unwrap();
    let committee = json::committee(&b["data"]["current_sync_committee"]).unwrap();
    let branch: Vec<B256> = serde_json::from_value(b["data"]["current_sync_committee_branch"].clone()).unwrap();
    bootstrap(chain_id, forks(d), gvr, checkpoint, &header, committee, &branch).expect("bootstrap verifies")
}

#[test]
fn sepolia_sync_committee_signature_and_finality_verify() {
    let Some(d) = dir() else { return };
    let mut c = setup(&d);
    println!("bootstrap ok: period {}, checkpoint {:?}", c.store.current_period, c.store.finalized_roots[0]);
    println!("participation {}/512", participants(&c.update.sync_committee_bits).len());
    let root = apply_finality_update(&mut c.store, &c.update, c.fork_version).expect("finality update verifies");
    println!("finalized beacon root VERIFIED by BLS: {root}");
}

#[test]
fn sepolia_finality_update_rejections() {
    let Some(d) = dir() else { return };
    let c = setup(&d);
    let try_ = |u: &FinalityUpdate, fv: [u8; 4]| apply_finality_update(&mut c.store.clone(), u, fv);

    // tampered attested header
    let mut u = json::finality_update(&read(&d, "finality_update.json")["data"]).unwrap();
    u.attested.proposer_index ^= 1;
    assert!(matches!(try_(&u, c.fork_version), Err(LcError::FinalityBranchInvalid | LcError::SignatureInvalid)));

    // tampered finalized header
    let mut u = json::finality_update(&read(&d, "finality_update.json")["data"]).unwrap();
    u.finalized.slot += 1;
    assert_eq!(try_(&u, c.fork_version), Err(LcError::FinalityBranchInvalid));

    // flip one signer bit
    let mut u = json::finality_update(&read(&d, "finality_update.json")["data"]).unwrap();
    let first_set = participants(&u.sync_committee_bits)[0];
    u.sync_committee_bits[first_set / 8] ^= 1 << (first_set % 8);
    assert_eq!(try_(&u, c.fork_version), Err(LcError::SignatureInvalid));

    // below 2/3
    let mut u = json::finality_update(&read(&d, "finality_update.json")["data"]).unwrap();
    for b in u.sync_committee_bits.iter_mut().skip(20) { *b = 0; }
    assert!(matches!(try_(&u, c.fork_version), Err(LcError::InsufficientParticipation(_))));

    // wrong fork version or genesis validators root
    let u = json::finality_update(&read(&d, "finality_update.json")["data"]).unwrap();
    assert_eq!(try_(&u, [0, 0, 0, 0]), Err(LcError::SignatureInvalid));
    let mut s = c.store.clone();
    s.genesis_validators_root = B256::repeat_byte(1);
    assert_eq!(apply_finality_update(&mut s, &u, c.fork_version), Err(LcError::SignatureInvalid));

    // wrong checkpoint
    let b = read(&d, "bootstrap.json");
    let header: attest_core::BeaconBlockHeader = serde_json::from_value(b["data"]["header"]["beacon"].clone()).unwrap();
    let committee = json::committee(&b["data"]["current_sync_committee"]).unwrap();
    let branch: Vec<B256> = serde_json::from_value(b["data"]["current_sync_committee_branch"].clone()).unwrap();
    assert_eq!(bootstrap(1, forks(&d), B256::ZERO, B256::repeat_byte(9), &header, committee.clone(), &branch).err(), Some(LcError::CheckpointMismatch));
    // the Gloas branch must not verify at the pre-Gloas position
    let cp: B256 = serde_json::from_value(b["checkpoint_root"].clone()).unwrap();
    assert_eq!(bootstrap(1, ForkSchedule { gloas_epoch: None }, B256::ZERO, cp, &header, committee, &branch).err(), Some(LcError::CommitteeBranchInvalid));
}

#[test]
fn sepolia_descrow_attested_with_bls_verified_anchor() {
    let Some(d) = dir() else { return };
    let mut c = setup(&d);
    apply_finality_update(&mut c.store, &c.update, c.fork_version).expect("finality");
    let l = loader::load_token_balance_claim(&d, U256::from(1u64)).unwrap();
    let st = attest(&l.claim, &c.store).expect("token balance attested");
    println!("token balance statement id {}", st.id);
    if d.join("proof_deal.json").exists() {
        for (label, claim) in loader::load_descrow_claims(&d, &l.claim).unwrap() {
            let st = attest(&claim, &c.store).unwrap_or_else(|e| panic!("{label}: {e}"));
            println!("DESCROW {label}: attested, id {}", st.id);
        }
    }
    let fresh = setup(&d).store;
    if !fresh.is_finalized(&l.finalized_beacon_root) {
        assert_eq!(attest(&l.claim, &fresh), Err(AttestError::HeaderNotFinalized));
    }
}

use attest_core::TrustedRoots;
