//! LEZ-side mint for the bridge's inbound leg (public path).
//!
//! A deposit stores keccak(DOMAIN, lezChainId, secret, recipient) in the vault. Minting
//! attests that record, checks the opening and spends a nullifier. The recipient is
//! inside the commitment, so a relayer can't redirect the mint.
//!
//! This path reveals which deposit funded which mint. The private path (prove an
//! opening of some commitment in the set, inside a private LEZ tx) is not built yet.

use alloy_primitives::{keccak256, Address, B256, U256};
use attest_core::{attest_many, AttestError, Claim, Cmp, Statement, Subject, TrustedRoots};
use serde::{Deserialize, Serialize};

pub const COMMITMENT_DOMAIN_TAG: &str = "LEZ-BRIDGE-DEPOSIT-v1";
pub const DEPOSITS_MAPPING_SLOT: u64 = 0;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BridgeError {
    #[error("B01 attestation failed: {0}")]
    Attestation(AttestError),
    #[error("B02 claim is not about the configured vault")]
    WrongVault,
    #[error("B03 claim does not cover the deposit record's slots")]
    WrongSlots,
    #[error("B04 commitment opening does not match")]
    BadOpening,
    #[error("B05 deposit already minted")]
    AlreadyMinted,
    #[error("B06 token not registered on this bridge")]
    UnsupportedToken,
    #[error("B07 malformed: {0}")]
    Malformed(&'static str),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeConfig {
    pub eth_chain_id: u64,
    pub vault: Address,
    pub lez_chain_id: u64,
    /// token -> wrapped mint id; address(0) is native ETH
    pub registry: Vec<(Address, B256)>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BridgeState {
    pub spent: Vec<B256>,
    pub supply: Vec<(B256, U256)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mint {
    pub recipient: B256,
    pub wrapped_mint: B256,
    pub amount: U256,
    pub deposit_id: u64,
    pub nullifier: B256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MintRequest {
    pub deposit_id: u64,
    /// deposits[id] slot 0 (commitment) and slot 1 (token | amount), both with Eq.
    pub commitment_claim: Claim,
    pub record_claim: Claim,
    pub secret: B256,
    pub recipient: B256,
}

pub fn deposit_base_slot(deposit_id: u64) -> U256 {
    let mut buf = [0u8; 64];
    buf[24..32].copy_from_slice(&deposit_id.to_be_bytes());
    buf[56..64].copy_from_slice(&DEPOSITS_MAPPING_SLOT.to_be_bytes());
    U256::from_be_bytes(keccak256(buf).0)
}

pub fn deposit_slots(deposit_id: u64) -> (B256, B256) {
    let base = deposit_base_slot(deposit_id);
    (B256::from(base), B256::from(base + U256::from(1u64)))
}

/// Same as LezVault.computeCommitment.
pub fn commitment(lez_chain_id: u64, secret: B256, recipient: B256) -> B256 {
    let mut buf = [0u8; 128];
    buf[..32].copy_from_slice(keccak256(COMMITMENT_DOMAIN_TAG.as_bytes()).as_slice());
    buf[56..64].copy_from_slice(&lez_chain_id.to_be_bytes());
    buf[64..96].copy_from_slice(secret.as_slice());
    buf[96..128].copy_from_slice(recipient.as_slice());
    keccak256(buf)
}

/// token in the low 20 bytes, amount (uint96) above it.
pub fn unpack_record(v: U256) -> (Address, U256) {
    let b = v.to_be_bytes::<32>();
    (Address::from_slice(&b[12..32]), U256::from_be_slice(&b[..12]))
}

fn eq_value(st: &Statement) -> Result<U256, BridgeError> {
    if st.cmp != Cmp::Eq {
        return Err(BridgeError::Malformed("deposit record claims must use Eq"));
    }
    Ok(st.rhs)
}

pub fn verify_and_mint(cfg: &BridgeConfig, req: &MintRequest, trusted: &impl TrustedRoots, state: &mut BridgeState) -> Result<Mint, BridgeError> {
    let sts = attest_many(&[req.commitment_claim.clone(), req.record_claim.clone()], trusted).map_err(|(_, e)| BridgeError::Attestation(e))?;
    let (c_st, r_st) = (&sts[0], &sts[1]);
    if c_st.chain_id != cfg.eth_chain_id {
        return Err(BridgeError::Attestation(AttestError::ChainIdMismatch));
    }
    if c_st.account != cfg.vault || r_st.account != cfg.vault {
        return Err(BridgeError::WrongVault);
    }
    let (s0, s1) = deposit_slots(req.deposit_id);
    if c_st.subject != Subject::StorageSlot(s0) || r_st.subject != Subject::StorageSlot(s1) || c_st.block_hash != r_st.block_hash {
        return Err(BridgeError::WrongSlots);
    }
    let committed = B256::from(eq_value(c_st)?);
    if committed.is_zero() || commitment(cfg.lez_chain_id, req.secret, req.recipient) != committed {
        return Err(BridgeError::BadOpening);
    }
    let (token, amount) = unpack_record(eq_value(r_st)?);
    let wrapped = cfg.registry.iter().find(|(t, _)| *t == token).map(|(_, m)| *m).ok_or(BridgeError::UnsupportedToken)?;
    // statement IDs don't depend on the block, so re-attesting later can't mint again
    let nullifier = c_st.id;
    if state.spent.contains(&nullifier) {
        return Err(BridgeError::AlreadyMinted);
    }
    state.spent.push(nullifier);
    match state.supply.iter_mut().find(|(m, _)| *m == wrapped) {
        Some((_, s)) => *s += amount,
        None => state.supply.push((wrapped, amount)),
    }
    Ok(Mint { recipient: req.recipient, wrapped_mint: wrapped, amount, deposit_id: req.deposit_id, nullifier })
}
