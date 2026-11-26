//! Execution header decoding. The hash is always recomputed from the RLP.

use crate::AttestError;
use alloy_primitives::{keccak256, B256};
use alloy_rlp::Header;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecHeader {
    pub hash: B256,
    pub state_root: B256,
    pub receipts_root: B256,
    pub number: u64,
}

impl ExecHeader {
    pub fn decode(rlp: &[u8]) -> Result<Self, AttestError> {
        let mut buf = rlp;
        let list = Header::decode(&mut buf).map_err(|_| AttestError::Malformed("header rlp"))?;
        if !list.list || list.payload_length != buf.len() {
            return Err(AttestError::Malformed("header rlp is not a single list"));
        }
        let mut items: Vec<&[u8]> = Vec::with_capacity(21);
        while !buf.is_empty() {
            let h = Header::decode(&mut buf).map_err(|_| AttestError::Malformed("header item"))?;
            if h.list || h.payload_length > buf.len() {
                return Err(AttestError::Malformed("header item"));
            }
            items.push(&buf[..h.payload_length]);
            buf = &buf[h.payload_length..];
        }
        if items.len() < 15 {
            return Err(AttestError::Malformed("header has too few fields"));
        }
        let b32 = |i: usize| -> Result<B256, AttestError> {
            if items[i].len() != 32 {
                return Err(AttestError::Malformed("header hash field length"));
            }
            Ok(B256::from_slice(items[i]))
        };
        let number_bytes = items[8];
        if number_bytes.len() > 8 || number_bytes.first() == Some(&0) {
            return Err(AttestError::Malformed("header number"));
        }
        let mut n = [0u8; 8];
        n[8 - number_bytes.len()..].copy_from_slice(number_bytes);
        Ok(ExecHeader {
            hash: keccak256(rlp),
            state_root: b32(3)?,
            receipts_root: b32(5)?,
            number: u64::from_be_bytes(n),
        })
    }
}
