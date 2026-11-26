//! Verifies a claim about finalized Ethereum state: beacon anchor, execution header,
//! account/storage proof, then a predicate over the proven value.

pub mod exec_header;
pub mod mpt;
pub mod ssz;

use alloy_primitives::{keccak256, Address, B256, U256};
use serde::{Deserialize, Serialize};

pub use exec_header::ExecHeader;
pub use mpt::{AccountProof, StorageProof};
pub use ssz::{BeaconBlockHeader, ExecutionPayloadHeader};

/// How the execution block is committed in the finalized beacon block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionAnchor {
    /// Capella to Fulu: full execution payload header at gindex 25.
    PayloadHeader { execution: ExecutionPayloadHeader, branch: Vec<B256> },
    /// Gloas: only the execution block hash (the bid's parent_block_hash).
    BlockHash { block_hash: B256, branch: Vec<B256> },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttestError {
    #[error("E01 header not finalized: beacon root is not in the instance's trusted set")]
    HeaderNotFinalized,
    #[error("E02 execution branch invalid: execution header is not part of the beacon block")]
    ExecutionBranchInvalid,
    #[error("E03 execution header hash mismatch")]
    ExecHeaderMismatch,
    #[error("E04 account inclusion proof invalid")]
    AccountProofInvalid,
    #[error("E05 storage inclusion proof invalid")]
    StorageProofInvalid,
    #[error("E06 predicate not satisfied")]
    PredicateFalse,
    #[error("E07 chain-ID mismatch")]
    ChainIdMismatch,
    #[error("E08 malformed input: {0}")]
    Malformed(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cmp {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
}

impl Cmp {
    pub fn holds(self, lhs: U256, rhs: U256) -> bool {
        match self {
            Cmp::Eq => lhs == rhs,
            Cmp::Ne => lhs != rhs,
            Cmp::Gt => lhs > rhs,
            Cmp::Gte => lhs >= rhs,
            Cmp::Lt => lhs < rhs,
            Cmp::Lte => lhs <= rhs,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Subject {
    AccountBalance,
    StorageSlot(B256),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Predicate {
    pub subject: Subject,
    pub cmp: Cmp,
    pub rhs: U256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub chain_id: u64,
    pub beacon_header: BeaconBlockHeader,
    pub execution: ExecutionAnchor,
    pub exec_header_rlp: alloy_primitives::Bytes,
    pub account: AccountProof,
    pub storage: Option<StorageProof>,
    pub predicate: Predicate,
}

/// Finalized beacon roots the instance has verified, plus its chain binding.
pub trait TrustedRoots {
    fn chain_id(&self) -> u64;
    fn is_finalized(&self, beacon_block_root: &B256) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Statement {
    pub chain_id: u64,
    pub beacon_block_root: B256,
    pub block_number: u64,
    pub block_hash: B256,
    pub account: Address,
    pub subject: Subject,
    pub cmp: Cmp,
    pub rhs: U256,
    /// Nullifier key for consumers. Independent of the block, see statement_id.
    pub id: B256,
}

/// keccak256("LEZ-ETH-ATTEST-v2" ‖ chain_id ‖ account ‖ subject ‖ slot ‖ value).
/// The block is left out on purpose: the same fact attested at two blocks must give
/// the same ID, or a consumer could act on one deposit twice.
pub fn statement_id(chain_id: u64, _block_hash: B256, account: Address, subject: Subject, value: U256) -> B256 {
    let (tag, slot) = match subject {
        Subject::AccountBalance => (0u8, B256::ZERO),
        Subject::StorageSlot(s) => (1u8, s),
    };
    let mut buf = Vec::with_capacity(17 + 8 + 20 + 1 + 32 + 32);
    buf.extend_from_slice(b"LEZ-ETH-ATTEST-v2");
    buf.extend_from_slice(&chain_id.to_be_bytes());
    buf.extend_from_slice(account.as_slice());
    buf.push(tag);
    buf.extend_from_slice(slot.as_slice());
    buf.extend_from_slice(&value.to_be_bytes::<32>());
    keccak256(&buf)
}

/// Inclusion and predicate only. Public so tests can show this alone is not enough.
pub fn verify_inclusion_and_predicate(
    state_root: B256,
    account: &AccountProof,
    storage: Option<&StorageProof>,
    predicate: &Predicate,
) -> Result<U256, AttestError> {
    mpt::verify_account(state_root, account)?;
    let value = match predicate.subject {
        Subject::AccountBalance => account.balance,
        Subject::StorageSlot(slot) => {
            let sp = storage.ok_or(AttestError::Malformed("storage proof required for StorageSlot"))?;
            if sp.key != slot {
                return Err(AttestError::Malformed("storage proof key does not match predicate slot"));
            }
            mpt::verify_storage(account.storage_hash, sp)?;
            sp.value
        }
    };
    if !predicate.cmp.holds(value, predicate.rhs) {
        return Err(AttestError::PredicateFalse);
    }
    Ok(value)
}

fn verify_anchor(claim: &Claim, trusted: &impl TrustedRoots) -> Result<(B256, ExecHeader), AttestError> {
    if claim.chain_id != trusted.chain_id() {
        return Err(AttestError::ChainIdMismatch);
    }

    let beacon_root = claim.beacon_header.hash_tree_root();
    if !trusted.is_finalized(&beacon_root) {
        return Err(AttestError::HeaderNotFinalized);
    }

    let committed_hash = match &claim.execution {
        ExecutionAnchor::PayloadHeader { execution, branch } => {
            if !ssz::verify_execution_branch(execution, branch, claim.beacon_header.body_root) {
                return Err(AttestError::ExecutionBranchInvalid);
            }
            execution.block_hash
        }
        ExecutionAnchor::BlockHash { block_hash, branch } => {
            if !ssz::verify_gloas_block_hash_branch(*block_hash, branch, claim.beacon_header.body_root) {
                return Err(AttestError::ExecutionBranchInvalid);
            }
            *block_hash
        }
    };

    let header = ExecHeader::decode(&claim.exec_header_rlp)?;
    if header.hash != committed_hash {
        return Err(AttestError::ExecHeaderMismatch);
    }
    if let ExecutionAnchor::PayloadHeader { execution, .. } = &claim.execution {
        if header.state_root != execution.state_root || header.number != execution.block_number {
            return Err(AttestError::ExecHeaderMismatch);
        }
    }
    Ok((beacon_root, header))
}

fn statement(claim: &Claim, beacon_root: B256, header: &ExecHeader, value: U256) -> Statement {
    Statement {
        chain_id: claim.chain_id,
        beacon_block_root: beacon_root,
        block_number: header.number,
        block_hash: header.hash,
        account: claim.account.address,
        subject: claim.predicate.subject,
        cmp: claim.predicate.cmp,
        rhs: claim.predicate.rhs,
        id: statement_id(claim.chain_id, header.hash, claim.account.address, claim.predicate.subject, value),
    }
}

fn eval_predicate(claim: &Claim) -> Result<U256, AttestError> {
    let value = match claim.predicate.subject {
        Subject::AccountBalance => claim.account.balance,
        Subject::StorageSlot(slot) => {
            let sp = claim.storage.as_ref().ok_or(AttestError::Malformed("storage proof required for StorageSlot"))?;
            if sp.key != slot {
                return Err(AttestError::Malformed("storage proof key does not match predicate slot"));
            }
            mpt::verify_storage(claim.account.storage_hash, sp)?;
            sp.value
        }
    };
    if !claim.predicate.cmp.holds(value, claim.predicate.rhs) {
        return Err(AttestError::PredicateFalse);
    }
    Ok(value)
}

pub fn attest(claim: &Claim, trusted: &impl TrustedRoots) -> Result<Statement, AttestError> {
    let (beacon_root, header) = verify_anchor(claim, trusted)?;
    let value = verify_inclusion_and_predicate(header.state_root, &claim.account, claim.storage.as_ref(), &claim.predicate)?;
    Ok(statement(claim, beacon_root, &header, value))
}

/// Same verdicts as calling `attest` per claim, but a shared anchor, header or account
/// proof is verified once. Fails on the first bad claim with its index.
pub fn attest_many(claims: &[Claim], trusted: &impl TrustedRoots) -> Result<Vec<Statement>, (usize, AttestError)> {
    let mut anchors: Vec<(&Claim, B256, ExecHeader)> = Vec::new();
    let mut accounts: Vec<(B256, &AccountProof)> = Vec::new();
    let mut out = Vec::with_capacity(claims.len());
    for (i, c) in claims.iter().enumerate() {
        let cached = anchors.iter().find(|(a, _, _)| {
            a.chain_id == c.chain_id && a.beacon_header == c.beacon_header && a.execution == c.execution && a.exec_header_rlp == c.exec_header_rlp
        });
        let (beacon_root, header) = match cached {
            Some((_, r, h)) => (*r, h.clone()),
            None => {
                let (r, h) = verify_anchor(c, trusted).map_err(|e| (i, e))?;
                anchors.push((c, r, h.clone()));
                (r, h)
            }
        };
        if !accounts.iter().any(|(sr, a)| *sr == header.state_root && *a == &c.account) {
            mpt::verify_account(header.state_root, &c.account).map_err(|e| (i, e))?;
            accounts.push((header.state_root, &c.account));
        }
        let value = eval_predicate(c).map_err(|e| (i, e))?;
        out.push(statement(c, beacon_root, &header, value));
    }
    Ok(out)
}

/// In-memory trusted set for tests and dry runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryRoots {
    pub chain_id: u64,
    pub roots: Vec<B256>,
}

impl TrustedRoots for MemoryRoots {
    fn chain_id(&self) -> u64 {
        self.chain_id
    }
    fn is_finalized(&self, r: &B256) -> bool {
        self.roots.contains(r)
    }
}

#[cfg(feature = "loader")]
pub mod loader;
