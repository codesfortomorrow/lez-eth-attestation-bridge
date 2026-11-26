//! Input: JSON (claims, trusted roots). Journal: (statements, error, cycles in attest_many).
use attest_core::{attest_many, Claim, MemoryRoots, Statement};
use risc0_zkvm::guest::env;

fn main() {
    let input: Vec<u8> = env::read();
    let (claims, trusted): (Vec<Claim>, MemoryRoots) = serde_json::from_slice(&input).expect("input json");
    let start = env::cycle_count();
    let result = attest_many(&claims, &trusted);
    let used = env::cycle_count() - start;
    match result {
        Ok(st) => env::commit(&(st, None::<String>, used)),
        Err((i, e)) => env::commit(&(Vec::<Statement>::new(), Some(format!("claim {i}: {e}")), used)),
    }
}
