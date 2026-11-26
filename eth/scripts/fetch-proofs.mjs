// Saves light-client data, the execution header and eth_getProof results for one
// finalized block into vectors/<NETWORK>/. Run ~15 min after the last transaction.
import { ethers } from "ethers";
import { need, provider, writeJson, readJson, VECTORS } from "./helpers.mjs";

const BEACON = need("BEACON_API").replace(/\/+$/, "");
const TOKEN = ethers.getAddress(need("TOKEN"));
const HOLDER = ethers.getAddress(need("HOLDER"));
const p = provider();

async function beacon(route) {
  const r = await fetch(BEACON + route, { headers: { accept: "application/json" } });
  if (!r.ok) throw new Error(`Beacon ${route} -> HTTP ${r.status}: ${(await r.text()).slice(0, 300)}`);
  return r.json();
}

const genesis = (await beacon("/eth/v1/beacon/genesis")).data;
const fork = (await beacon("/eth/v1/beacon/states/head/fork")).data;
writeJson("genesis.json", { genesis, fork });

const fu = await beacon("/eth/v1/beacon/light_client/finality_update");
writeJson("finality_update.json", fu);
const fin = fu.data.finalized_header;
// Gloas headers carry only execution_block_hash; earlier forks the full payload header
const layout = fin.execution ? "payload_header" : fin.execution_block_hash ? "gloas_block_hash" : null;
if (!layout) throw new Error(`finalized_header has neither execution nor execution_block_hash (version ${fu.version}); keys: ${Object.keys(fin)}`);
const committedHash = layout === "payload_header" ? fin.execution.block_hash : fin.execution_block_hash;
console.log(`Light-client version ${fu.version}, layout ${layout}`);
console.log(`Finalized beacon slot ${fin.beacon.slot}`);
console.log(`  committed execution block_hash ${committedHash}`);
console.log(`  sync participation   ${countBits(fu.data.sync_aggregate.sync_committee_bits)}/512`);

const cps = (await beacon("/eth/v1/beacon/states/head/finality_checkpoints")).data;
const bootRoot = cps.finalized.root;
try {
  writeJson("bootstrap.json", { checkpoint_root: bootRoot, ...(await beacon(`/eth/v1/beacon/light_client/bootstrap/${bootRoot}`)) });
} catch (e) {
  console.warn("WARN bootstrap not served by this endpoint (only needed for the finality stretch):", e.message);
}
const period = Math.floor(Number(fin.beacon.slot) / (32 * 256));
try {
  writeJson("updates.json", { period, updates: await beacon(`/eth/v1/beacon/light_client/updates?start_period=${period}&count=1`) });
} catch (e) {
  console.warn("WARN light_client/updates not served (only needed for the finality stretch):", e.message);
}

const h = await p.send("eth_getBlockByHash", [committedHash, false]);
if (!h) throw new Error(`Execution RPC does not know block ${committedHash}`);
if (h.hash.toLowerCase() !== committedHash.toLowerCase())
  throw new Error(`Execution RPC block hash ${h.hash} != beacon-committed hash ${committedHash}`);
const finBlock = BigInt(h.number);
const tag = ethers.toQuantity(finBlock);
const { fields, rlp } = headerRlp(h);
const { transactions, uncles, withdrawals, size, totalDifficulty, ...headerOnly } = h;
writeJson("exec_header.json", { header: headerOnly, rlp, field_order: fields });
console.log(`Execution block ${finBlock}: header RLP verified (${fields.length} fields: ...${fields.slice(15).join(", ")})`);

const erc20 = new ethers.Contract(TOKEN, ["function balanceOf(address) view returns (uint256)", "function decimals() view returns (uint8)"], p);
if ((await p.getCode(TOKEN, tag)) === "0x") {
  const live = (await p.getCode(TOKEN)) !== "0x";
  const head = await p.getBlockNumber();
  throw new Error(live
    ? `TOKEN ${TOKEN} exists now but NOT yet at finalized block ${finBlock} (head is ${head}, ~${head - Number(finBlock)} blocks ahead). Wait ~15 minutes after deploying/funding, then re-run.`
    : `No contract at TOKEN ${TOKEN} on chain ${(await p.getNetwork()).chainId}. Check TOKEN in eth/.env (must be the contract address printed by deploy:token).`);
}
const bal = await erc20.balanceOf(HOLDER, { blockTag: tag });
if (bal === 0n) throw new Error(`HOLDER has 0 balance at finalized block ${finBlock}. Fund it, wait ~15 min, re-run.`);
const coder = ethers.AbiCoder.defaultAbiCoder();
let mappingSlot = -1, storageKey;
for (let i = 0; i < 20; i++) {
  const key = ethers.keccak256(coder.encode(["address", "uint256"], [HOLDER, i]));
  if (BigInt(await p.getStorage(TOKEN, key, tag)) === bal) { mappingSlot = i; storageKey = key; break; }
}
if (mappingSlot < 0) throw new Error("Could not find the balances mapping slot in 0..19 (non-standard token layout?)");

const proofToken = await p.send("eth_getProof", [TOKEN, [storageKey], tag]);
const proofHolder = await p.send("eth_getProof", [HOLDER, [], tag]);
writeJson("proof_token.json", proofToken);
writeJson("proof_holder.json", proofHolder);

const descrow = readJson("descrow.json");
let dealInfo = null;
if (descrow && ethers.getAddress(descrow.deal) === HOLDER) {
  const keys = [descrow.slots.escrowedAmount, descrow.slots.status];
  const proofDeal = await p.send("eth_getProof", [HOLDER, keys, tag]);
  const val = (k) => BigInt(proofDeal.storageProof.find((x) => BigInt(x.key) === BigInt(k)).value);
  const escrowed = val(descrow.slots.escrowedAmount), status = val(descrow.slots.status);
  if (escrowed === 0n || status !== 1n)
    throw new Error(`At finalized block ${finBlock} the deal is not funded yet (escrowed ${escrowed}, status ${status}). Wait ~15 min after fundMilestone, re-run.`);
  writeJson("proof_deal.json", proofDeal);
  dealInfo = { escrowed_amount: escrowed, status, slots: descrow.slots };
  console.log(`DESCROW deal milestone 0: status ${status} (Funded), escrowedAmount ${ethers.formatUnits(escrowed, 6)} tUSD`);
} else {
  console.warn("NOTE: HOLDER is not the DESCROW deal from descrow.json (run `npm run escrow` and set HOLDER to the deal).");
}

const bridge = readJson("bridge.json");
if (bridge) {
  const keys = bridge.deposits.flatMap((d) => d.slots);
  const pv = await p.send("eth_getProof", [bridge.vault, keys, tag]);
  for (const d of bridge.deposits) {
    const v = pv.storageProof.find((x) => BigInt(x.key) === BigInt(d.slots[0]));
    if (!v || BigInt(v.value) === 0n) throw new Error(`Vault deposit #${d.id} not finalized yet at block ${finBlock}; wait ~15 min after the deposit, re-run.`);
  }
  writeJson("proof_vault.json", pv);
  console.log(`LezVault: ${bridge.deposits.length} deposit records proven at block ${finBlock}`);
}

const chainId = (await p.getNetwork()).chainId;
writeJson("meta.json", {
  network: process.env.NETWORK || "sepolia",
  chain_id: chainId,
  token: TOKEN,
  holder: HOLDER,
  token_decimals: Number(await erc20.decimals()),
  balances_mapping_slot: mappingSlot,
  storage_key: storageKey,
  expected_balance: bal,
  block_number: finBlock,
  block_hash: h.hash,
  state_root: h.stateRoot,
  beacon_slot: fin.beacon.slot,
  light_client_version: fu.version,
  descrow: dealInfo,
  execution_layout: layout,
  fetched_at: new Date().toISOString(),
});

console.log(`\nHOLDER ${HOLDER} holds ${ethers.formatUnits(bal, 6)} tUSD at block ${finBlock} (mapping slot ${mappingSlot})`);
console.log(`Vectors written to ${VECTORS}`);
console.log("Send the whole vectors/ folder (zip it) back to Claude.");

function countBits(hex) {
  let n = 0;
  for (const b of ethers.getBytes(hex)) for (let x = b; x; x >>= 1) n += x & 1;
  return n;
}

// Unknown trailing fields from newer forks are tried in each order until the hash matches.
function headerRlp(b) {
  const order = [
    ["parentHash", "b"], ["sha3Uncles", "b"], ["miner", "b"], ["stateRoot", "b"],
    ["transactionsRoot", "b"], ["receiptsRoot", "b"], ["logsBloom", "b"], ["difficulty", "i"],
    ["number", "i"], ["gasLimit", "i"], ["gasUsed", "i"], ["timestamp", "i"], ["extraData", "b"],
    ["mixHash", "b"], ["nonce", "b"], ["baseFeePerGas", "i"], ["withdrawalsRoot", "b"],
    ["blobGasUsed", "i"], ["excessBlobGas", "i"], ["parentBeaconBlockRoot", "b"], ["requestsHash", "b"],
  ];
  const notHeader = new Set(["hash", "size", "totalDifficulty", "transactions", "uncles", "withdrawals", "sealFields", "author"]);
  const known = new Set(order.map(([k]) => k));
  const int = (v) => (BigInt(v) === 0n ? "0x" : ethers.toBeHex(BigInt(v)));
  const enc = (k, t) => (t === "i" ? int(b[k]) : b[k]);
  const base = [], fields = [];
  for (const [k, t] of order) {
    if (b[k] === undefined || b[k] === null) {
      if (fields.length >= 15) break;
      throw new Error(`Header field ${k} missing`);
    }
    fields.push(k);
    base.push(enc(k, t));
  }
  const extras = Object.keys(b).filter((k) => !known.has(k) && !notHeader.has(k) && b[k] !== null && typeof b[k] === "string");
  const typed = extras.map((k) => [k, /^0x[0-9a-fA-F]{64}$/.test(b[k]) ? "b" : "i"]);
  for (const perm of permutations(typed)) {
    const rlp = ethers.encodeRlp([...base, ...perm.map(([k, t]) => enc(k, t))]);
    if (ethers.keccak256(rlp) === b.hash) return { fields: [...fields, ...perm.map(([k]) => k)], rlp };
  }
  throw new Error(`Could not reproduce block hash from header fields. Extra fields seen: [${extras}]. Send this message to Claude.`);
}

function* permutations(a) {
  if (a.length <= 1) { yield a; return; }
  for (let i = 0; i < a.length; i++)
    for (const rest of permutations([...a.slice(0, i), ...a.slice(i + 1)])) yield [a[i], ...rest];
}
