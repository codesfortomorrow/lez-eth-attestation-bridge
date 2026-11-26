//! Cycle counts for the attestation inside the RISC Zero executor.
//! RISC0_DEV_MODE=1 cargo run --release -p host -- ../../vectors/sepolia [--prove]
use alloy_primitives::{B256, U256};
use attest_core::{loader, Claim, MemoryRoots, Statement};
use methods::{ATTEST_ELF, ATTEST_ID};
use risc0_zkvm::{default_executor, default_prover, ExecutorEnv};
use std::{path::PathBuf, time::Instant};

/// Assumed LEZ per-transaction budget; confirm on testnet 0.4.
const LEZ_TX_CYCLE_BUDGET: u64 = 10_000_000;

fn run(label: &str, claims: &[Claim], trusted: &MemoryRoots, prove: bool) -> anyhow::Result<u64> {
    let input = serde_json::to_vec(&(claims, trusted))?;
    let env = ExecutorEnv::builder().write(&input)?.build()?;
    let t = Instant::now();
    let info = default_executor().execute(env, ATTEST_ELF)?;
    let exec_ms = t.elapsed().as_millis();
    let (st, err, cycles): (Vec<Statement>, Option<String>, u64) = info.journal.decode()?;
    let total: u64 = info.segments.iter().map(|s| s.cycles as u64).sum();
    println!(
        "| {label:<38} | {:>6} | {:>10} | {:>9} | {:>5.1}% | {:>10} | {:>6} | {} |",
        claims.len(),
        cycles,
        cycles / claims.len().max(1) as u64,
        cycles as f64 * 100.0 / LEZ_TX_CYCLE_BUDGET as f64,
        total,
        exec_ms,
        match &err { None => format!("OK ({} statements, first id {})", st.len(), st.first().map(|s| s.id.to_string()[..14].to_string()).unwrap_or_default()), Some(e) => format!("REJECTED {e}") }
    );
    if prove {
        let env = ExecutorEnv::builder().write(&input)?.build()?;
        let t = Instant::now();
        let receipt = default_prover().prove(env, ATTEST_ELF)?.receipt;
        receipt.verify(ATTEST_ID)?;
        let size = bincode_len(&receipt);
        println!("  proved + verified in {:.1}s, receipt ~{} KB", t.elapsed().as_secs_f64(), size / 1024);
    }
    Ok(cycles)
}

fn bincode_len(r: &risc0_zkvm::Receipt) -> usize {
    serde_json::to_vec(r).map(|v| v.len()).unwrap_or(0)
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(args.get(1).cloned().unwrap_or("../../vectors/sepolia".into()));
    let prove = args.iter().any(|a| a == "--prove");

    // light client runs natively here; measuring it in the zkVM is still to do
    let t = Instant::now();
    let store = light_client::verify_from_vectors(&dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("light client: bootstrap + finality update verified natively in {:?}", t.elapsed());
    let l = loader::load_token_balance_claim(&dir, U256::from(1u64)).map_err(anyhow::Error::msg)?;
    anyhow::ensure!(store.finalized_roots.contains(&l.finalized_beacon_root), "claim's beacon root was not BLS-verified");
    let trusted = MemoryRoots { chain_id: store.chain_id, roots: store.finalized_roots.clone() };

    println!("| claims                                 | count  | verify cyc | per claim | budget | total cyc  | ms     | result |");
    println!("|----------------------------------------|-------:|-----------:|----------:|-------:|-----------:|-------:|--------|");
    let mut all = vec![l.claim.clone()];
    run("tUSD.balances[deal] >= 1", &all, &trusted, prove)?;
    if dir.join("proof_deal.json").exists() {
        for (label, c) in loader::load_descrow_claims(&dir, &l.claim).map_err(anyhow::Error::msg)? {
            run(&format!("DESCROW {label}"), std::slice::from_ref(&c), &trusted, prove)?;
            all.push(c);
        }
        run("BATCH: all 3 escrow facts, same block", &all, &trusted, prove)?;
    }
    let empty = MemoryRoots { chain_id: l.claim.chain_id, roots: vec![B256::repeat_byte(1)] };
    run("fabricated anchor (must reject)", std::slice::from_ref(&l.claim), &empty, false)?;
    println!("\nverify cyc = cycles inside attest_many() only (env::cycle_count); total cyc includes JSON input decoding.");
    println!("Budget column assumes {LEZ_TX_CYCLE_BUDGET} cycles per LEZ transaction; confirm against LEZ testnet 0.4.");
    println!("Run with RISC0_INFO=1 to see precompile (sha2 / keccak) usage statistics.");
    Ok(())
}
