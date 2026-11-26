# cft-lez-attestation

Proof of concept for Logos **RFP-021** (trustless Ethereum state attestation for LEZ) and the
inbound leg of **RFP-022** (privacy-preserving wrapped ERC-20 and ETH bridge).

It proves facts about finalized Ethereum state inside a LEZ / RISC Zero program, using only
Ethereum's own data: the light-client checkpoint, sync-committee signatures, the execution
header and `eth_getProof`. No relayer, multisig or oracle is trusted.

Everything below runs against **live Sepolia after the Glamsterdam (Gloas) upgrade**.

**At a glance**

| | |
|---|---|
| Finality | 493/512 sync-committee signatures, BLS aggregate verified |
| Attested on Sepolia | DESCROW deal `Funded`, 980 tUSD escrowed; vault deposits of 1000 mUSDT and 0.01 ETH, each minted exactly once |
| zkVM cost per claim | **~188k cycles** (about 1.9% of an assumed 10M budget), down from 1.6M with RISC Zero precompiles |
| Batch of 3 claims | 404,739 cycles (2.2x one claim, not 3x) |
| Fabricated chain | rejected in 9,559 cycles |
| Reproduce | `cargo test --release` against committed vectors, no keys or RPC (see [Quick check](#quick-check-no-wallet-rpc-or-keys-needed)) |

Details in [Measurements](#measurements) and [What is tested](#what-is-tested).

Licensed MIT OR Apache-2.0.

---

## What it proves

1. **A DESCROW escrow is funded.** For a real DESCROW v2.2 deal on Sepolia:
   milestone 0 `status == Funded`, `escrowedAmount == 980 tUSD`, and the token contract
   shows the deal holding at least 980 tUSD.
2. **A bridge deposit exists, and can be minted exactly once.** For deposits into `LezVault`
   (1000 mUSDT and 0.01 ETH), LEZ-side logic attests the deposit record, checks the claimant's
   opening of the hidden-recipient commitment and spends a nullifier.

The chain of trust for both:

```
checkpoint root (only trusted input)
  -> current sync committee                  Merkle branch in the checkpoint state
  -> finality update signed by >= 342/512    BLS12-381 aggregate signature
  -> finalized beacon block                  finality branch
  -> execution block hash                    SSZ branch in the beacon body (Gloas layout)
  -> execution header -> state root          keccak(rlp(header))
  -> contract account -> storage slot        eth_getProof (Merkle-Patricia)
  -> predicate                               ==, !=, <, <=, >, >=
```

## Live Sepolia deployments

| Contract | Address |
|---|---|
| TestUSD (tUSD) | `0x8709093fd981176Ac638f08ff97DaFF776c0Dd91` |
| DESCROW EscrowFactory | `0x89c1695F175Ee0B47b9EC91042Dca0248DacDf55` |
| DESCROW deal | `0x18cd749EDC3753EA8c9555a26102Ec338232FB02` |
| LezVault | `0x5814a30d1eA2C5596430f07cD17c4A6ff4f0ca6a` |
| MockUSDT (no-bool transfers, like USDT) | `0x486a602AcD311F5b4Cf69283761cE5335d211743` |

Proof data from two finalized blocks is committed in `vectors/`:

| Folder | Finalized block | Contents |
|---|---|---|
| `vectors/sepolia-first-run` | 12211799 | DESCROW deal (used for the zkVM measurements) |
| `vectors/sepolia` | 12212386 | DESCROW deal and both vault deposits |

## Layout

| Path | What |
|---|---|
| `crates/attest-core` | The verifier: beacon anchor, execution header, account and storage proofs, predicates, batching |
| `crates/light-client` | Sync-committee light client (bootstrap, BLS-verified finality updates) |
| `crates/bridge-core` | Bridge mint logic on top of attest-core: vault binding, commitment opening, nullifiers |
| `crates/zkvm` | RISC Zero guest and host for cycle counts |
| `eth/contracts` | `TestUSD`, `bridge/LezVault`, `bridge/MockUSDT`; `descrow/` is DESCROW v2.2, unmodified |
| `eth/scripts` | Deploy, create the escrow, make bridge deposits, fetch proofs |
| `vectors/` | Live Sepolia proof data the tests run against |

---

## Quick check (no wallet, RPC or keys needed)

The live Sepolia proof data is committed in `vectors/`, so the whole verification can be
re-run from a fresh clone:

```bash
cd crates/attest-core  && cargo test --release
cd ../light-client     && cargo test --release
cd ../bridge-core      && cargo test --release
```

Prerequisites: Rust 1.85 or newer (edition 2024) and a C compiler for `blst`
(`build-essential` on Debian/Ubuntu, Xcode command line tools on macOS).

---

## Running it

### Ethereum side (Node 18+)

These scripts deploy your own copies of the contracts with your wallet, so the addresses will
differ from the ones above. You need Sepolia ETH (about 0.02) for gas, the test seller and the
ETH deposit.

```bash
cd eth
npm install
cp .env.example .env        # EXEC_RPC (must support eth_getProof), BEACON_API, PRIVATE_KEY

npm run deploy:token        # TestUSD; put the printed address in .env as TOKEN
npm run escrow              # DESCROW factory + deal, seller acknowledges, buyer funds milestone 0
                            # put the printed deal address in .env as HOLDER
npm run bridge              # LezVault + MockUSDT, deposits of 1000 mUSDT and 0.01 ETH
# wait ~15 minutes for finality
npm run fetch               # writes vectors/sepolia/*.json
```

To send test tUSD to another address, set `SEND_TO` (and optionally `AMOUNT`) and run
`npm run send`. It is a plain transfer, not an escrow.

`BEACON_API` must serve the light-client routes. `https://lodestar-sepolia.chainsafe.io` works.

`npm run escrow` and `npm run bridge` can be re-run; they pick up where they stopped. Their
state files (`.descrow-state.json`, `.bridge-state.json`) are gitignored, and the first one
holds the test seller's private key SO BE CAREFUL.

What `fetch` checks before saving anything:

- the execution block hash from the RPC equals the hash committed in the beacon block
- `keccak256(rlp(header))` recomputed locally equals that hash (new header fields from later
  forks are handled by trying their order until the hash matches)
- the token's balances slot is found by matching storage against `balanceOf`, not assumed
- the deal's and the vault's slots hold the expected values at the finalized block

### Rust tests

```bash
cd crates/attest-core  && cargo test
cd crates/light-client && cargo test --release
cd crates/bridge-core  && cargo test --release
```

The Sepolia tests use `vectors/sepolia` and are skipped if it is missing. The Hardhat tests
take a proof dump from a local node via `HARDHAT_PROOFS` / `HARDHAT_BRIDGE`.

### zkVM cycle counts (Linux, macOS or WSL2)

```bash
curl -L https://risczero.com/install | bash && rzup install
cd crates/zkvm
RISC0_DEV_MODE=1 cargo run --release -p host -- ../../vectors/sepolia
```

Add `--prove` for a real proof. `RISC0_INFO=1` prints precompile usage.

---

## What is tested

**Verifier (attest-core).** 20 tests, offline, on a Hardhat node and on live Sepolia.

- honest claims attest; all six comparison operators
- **fabricated chain:** a fully self-consistent fake Ethereum (valid tries, valid header hash,
  valid execution branch) passes the inclusion check on its own but is rejected by `attest`,
  because its beacon root was never finalized on the instance
- tampered storage value, tampered proof node, tampered storage root, tampered committed
  block hash or state root, swapped execution header: rejected
- wrong chain ID in either direction, predicate over a slot the proof doesn't cover: rejected
- statement IDs are deterministic, differ for different state, and are the same for the same
  fact attested at different blocks
- batched attestation gives the same statements as one-by-one, and reports the failing claim
- execution header decoding checked against Ethereum mainnet's genesis hash

**Light client.** 6 tests, with 512 generated keys and on live Sepolia.

- bootstrap from the checkpoint, committee proven in state
- live Sepolia finality update: **493/512 signatures, BLS aggregate verified**
- 342 signers accepted, 341 rejected
- signer bits that don't match the signature, keys outside the committee, tampered attested
  or finalized header, wrong fork version, wrong genesis validators root, wrong checkpoint,
  signature from another period, Gloas branch at the pre-Gloas position: all rejected

**Bridge mint.** On a Hardhat node and on live Sepolia (under the BLS-verified anchor).

- each deposit mints once, to the committed recipient, for the deposited amount
- wrapped supply equals what the vault holds
- double mint, relayer redirecting to itself, wrong secret, one deposit's opening used on
  another, wrong deposit id, different vault, inflated amount, unfinalized block,
  unregistered token: all rejected, and a rejected claim spends nothing

**Contracts and scripts.**

- MockUSDT (no return value from `transfer`/`transferFrom`) deposits through SafeERC20
- the deposit script checks that the LEZ recipient appears nowhere in calldata or logs
- DESCROW storage slots are checked against the contract's own getters before being proven

---

## Measurements

RISC Zero executor (risc0-zkvm 3.1, `RISC0_DEV_MODE=1`), data from `vectors/sepolia-first-run`.
Cycles are counted with `env::cycle_count()` around the verifier only. The budget column
assumes 10,000,000 cycles per LEZ transaction, still to be confirmed on LEZ testnet 0.4.

| Claim | Software hashes | RISC Zero sha256 + keccak precompiles | Budget |
|---|---:|---:|---:|
| `tUSD.balances[deal] >= 1` | 1,635,232 | **188,070** | 1.9% |
| DESCROW `escrowedAmount == 980 tUSD` | 1,598,737 | **184,386** | 1.8% |
| DESCROW `status == Funded` | 1,598,083 | **183,691** | 1.8% |
| All three, batched | — | **404,739** (134,913 each) | 4.0% |
| Fabricated anchor (rejected) | 64,386 | **9,559** | 0.1% |

Verdicts are identical natively, in the plain guest and in the accelerated guest.

Hardware: Intel Core i3-1315U laptop (2 performance + 4 efficiency cores), WSL2 Ubuntu with a
3 GB memory limit. Each executor run took 177 to 419 ms. The light-client step ran natively in
55 to 74 ms.

About 1.9M further cycles per run go to decoding the JSON test input inside the guest. A LEZ
program takes binary instruction data, so that cost is not part of the verifier.

Not measured yet: the light-client update inside the zkVM, real proving time, proof size and
peak memory (proving needs more than the 3 GB available here).

---

## Design decisions

**D-001 One verification core for every path.** `attest-core` is plain Rust and is used
natively (tests, dry runs), in the RISC Zero guest, and will be used by the LEZ program, so
every path gives the same verdicts and error codes. It avoids anything a zkVM can't run.

**D-002 Reuse audited primitives.** Merkle-Patricia proofs use `alloy-trie`, BLS uses `blst`,
RLP uses `alloy-rlp`. Our own code is limited to SSZ branch checks and glue.

**D-003 Prove escrow state, not just a balance.** A plain transfer raises a contract's balance
without creating an escrow, so we attest DESCROW's own `status` and `escrowedAmount` together
with the token balance, at one block.

**D-004 Gloas generalized indices.** Sepolia runs Gloas since epoch 353024. Its light-client
headers carry only `execution_block_hash` (EIP-7732), and EIP-7688 progressive containers mix
an active-fields bitvector into container roots, so the branches are deeper than the earlier
values we started from (832 / 169 / 86). For each live branch exactly one position verifies:

| Proof | Generalized index (depth) |
|---|---|
| `signed_execution_payload_bid.message.parent_block_hash` in BeaconBlockBody | 2856 (11) |
| `finalized_checkpoint.root` in BeaconState | 735 (9) |
| `current_sync_committee` in BeaconState | 2945 (11) |

The index is chosen from the header's epoch using the instance's fixed fork schedule, never
from the branch length, so a submitter can't aim a branch at a different field. To do: confirm
against the consensus-specs release Sepolia clients run, and add `next_sync_committee` for
period handoffs.

**D-005 Liveness, not trust.** The checkpoint root is the only trusted input; everything after
it is verified. Whoever fetches and submits data can withhold it but can't make a false claim
verify.

**D-006 Statement ID does not depend on the block.**
`id = keccak256("LEZ-ETH-ATTEST-v2" ‖ chain_id ‖ account ‖ subject ‖ slot ‖ value)`.
An earlier version included the block hash, which gave the same deposit a different ID at each
block and would have allowed a double mint through a nullifier keyed on it. Consumers should
know that a slot whose value changes and later returns to an old value gets the old ID back;
vault deposit records are written once, so that can't happen there.

**D-007 Bridge scope in this PoC.** Built: `LezVault` (native ETH without wrapping, USDT-style
tokens, fixed denominations, rolling per-token cap, per-token and global freeze, exact-amount
check against fee-on-transfer tokens; a deposit stores only
`keccak(DOMAIN, lezChainId, secret, recipient)`, with the secret derived from a wallet
signature so it can be recovered from the seed) and the public mint path in `bridge-core`.
Not built: the private mint path, burn on LEZ and release on Ethereum, relayer fees, the
failsafe, Logos modules and UI. The public path reveals which deposit funded which mint, which
is why the full design defaults to the private path.

---

## Not built yet

- the LEZ program itself (the guest is the measurement harness for the same code)
- light-client update and committee handoff inside the zkVM, and the precompile comparison
- binary instruction input, proving time and memory on a mid-range and a minimum-spec laptop
- event-log inclusion via the receipts root (account and storage slots are done)
- RFP-022: private mint path, burn and release, relayers, failsafe. The private mint will publish
  `H(secret, statement_id)` as its nullifier: the statement ID alone names the vault slot and
  would link a private mint to its deposit
- Logos core and UI modules, CLI, IDL, operator daemon
- third-party audit