// Deploys MockUSDT and LezVault, registers mUSDT and ETH, and makes two deposits.
// Each deposit stores only a commitment; the LEZ recipient never appears on Ethereum.
// Re-runnable: progress is kept in eth/.bridge-state.json.
import { ethers } from "ethers";
import fs from "node:fs";
import path from "node:path";
import { compile, wallet, writeJson, ROOT } from "./helpers.mjs";

const STATE = path.join(ROOT, ".bridge-state.json");
const st = fs.existsSync(STATE) ? JSON.parse(fs.readFileSync(STATE, "utf8")) : {};
const save = () => fs.writeFileSync(STATE, JSON.stringify(st, null, 2));

const base = wallet();
const p = base.provider;
const w = new ethers.NonceManager(base);
const me = base.address;
const chainId = (await p.getNetwork()).chainId;
const LEZ_CHAIN_ID = BigInt(process.env.LEZ_CHAIN_ID || "0x4c455a"); // placeholder LEZ zone id ("LEZ")
console.log(`chain ${chainId}  user ${me}  LEZ zone id ${LEZ_CHAIN_ID}`);

const U = compile("bridge/MockUSDT.sol", "MockUSDT");
const V = compile("bridge/LezVault.sol", "LezVault");
const deploy = async (key, C, args, label) => {
  if (st[key]) { console.log(`${label} ${st[key]} (reused)`); return new ethers.Contract(st[key], C.abi, w); }
  const c = await new ethers.ContractFactory(C.abi, C.bytecode, w).deploy(...args);
  console.log(`deploying ${label}: ${c.deploymentTransaction().hash}`);
  await c.waitForDeployment();
  st[key] = await c.getAddress(); save();
  console.log(`${label} ${st[key]}`);
  return c;
};
const usdt = await deploy("usdt", U, [], "MockUSDT");
const vault = await deploy("vault", V, [me, me], "LezVault");
const send = async (label, txp) => { const tx = await txp; console.log(`${label}: ${tx.hash}`); return tx.wait(); };

const D_USDT = ethers.parseUnits("1000", 6), D_ETH = ethers.parseEther("0.01");
if (!st.registered) {
  await send("register mUSDT", vault.registerToken(st.usdt, ethers.parseUnits("1000000", 6), [ethers.parseUnits("100", 6), D_USDT]));
  await send("register ETH", vault.registerToken(ethers.ZeroAddress, ethers.parseEther("1"), [D_ETH]));
  st.registered = true; save();
}

// Secret comes from a signature, so it can be re-derived from the seed.
async function secretFor(i) {
  const sig = await base.signMessage(`LEZ bridge deposit secret v1\nvault ${st.vault}\nchain ${chainId}\nindex ${i}`);
  return ethers.keccak256(sig);
}
// placeholder LEZ account ids
const recipients = [ethers.id("cft-demo-lez-account-alice"), ethers.id("cft-demo-lez-account-bob")];
const coder = ethers.AbiCoder.defaultAbiCoder();
const DOMAIN = ethers.id("LEZ-BRIDGE-DEPOSIT-v1");
const commit = (secret, r) => ethers.keccak256(coder.encode(["bytes32", "uint256", "bytes32", "bytes32"], [DOMAIN, LEZ_CHAIN_ID, secret, r]));

st.deposits ??= [];
const plan = [
  { token: st.usdt, amount: D_USDT, label: "1000 mUSDT" },
  { token: ethers.ZeroAddress, amount: D_ETH, label: "0.01 ETH" },
];
for (let i = 0; i < plan.length; i++) {
  if (st.deposits[i]) continue;
  const secret = await secretFor(i);
  const c = commit(secret, recipients[i]);
  if (c !== (await vault.computeCommitment(LEZ_CHAIN_ID, secret, recipients[i]))) throw new Error("commitment mismatch JS vs Solidity");
  if (plan[i].token !== ethers.ZeroAddress) {
    await send("mint mUSDT", usdt.mint(me, plan[i].amount));
    await send("approve mUSDT", usdt.approve(st.vault, plan[i].amount));
  }
  const rc = await send(`deposit ${plan[i].label}`, vault.deposit(plan[i].token, plan[i].amount, c, { value: plan[i].token === ethers.ZeroAddress ? plan[i].amount : 0n }));
  const ev = rc.logs.map((l) => { try { return vault.interface.parseLog(l); } catch { return null; } }).find((e) => e?.name === "Deposited");
  st.deposits[i] = { id: Number(ev.args.id), token: plan[i].token, amount: plan[i].amount.toString(), commitment: c, block: rc.blockNumber, tx: rc.hash };
  save();
}

// deposits[id]: keccak(id, 0) -> commitment, +1 -> token | amount
const slotsFor = (id) => { const b = BigInt(ethers.keccak256(coder.encode(["uint256", "uint256"], [id, 0]))); return [ethers.toBeHex(b, 32), ethers.toBeHex(b + 1n, 32)]; };
const out = [];
for (let i = 0; i < st.deposits.length; i++) {
  const d = st.deposits[i];
  const [s0, s1] = slotsFor(d.id);
  const v0 = await p.getStorage(st.vault, s0), v1 = await p.getStorage(st.vault, s1);
  const packed = (BigInt(d.amount) << 160n) | BigInt(d.token);
  if (v0 !== d.commitment || BigInt(v1) !== packed) throw new Error(`storage check failed for deposit ${d.id}`);
  out.push({ ...d, slots: [s0, s1], record_value: ethers.toBeHex(packed, 32),
             test_only_secret: await secretFor(i), test_only_recipient: recipients[i] });
}
// make sure no recipient leaked into calldata or logs
for (const d of st.deposits) {
  const tx = await p.getTransaction(d.tx), rc = await p.getTransactionReceipt(d.tx);
  const blob = (tx.data + rc.logs.map((l) => l.data + l.topics.join("")).join("")).toLowerCase();
  for (const r of recipients) if (blob.includes(r.slice(2).toLowerCase())) throw new Error("recipient leaked on Ethereum!");
}
writeJson("bridge.json", {
  note: "PoC test deposits on Sepolia. test_only_* fields are included so the Rust tests can open the commitments; real clients re-derive the secret from the wallet signature and never publish it.",
  eth_chain_id: chainId, lez_chain_id: LEZ_CHAIN_ID, vault: st.vault, usdt: st.usdt, deposits: out,
});
const vbal = await p.getBalance(st.vault), ubal = await new ethers.Contract(st.usdt, ["function balanceOf(address) view returns (uint256)"], p).balanceOf(st.vault);
console.log(`\nLezVault ${st.vault} holds ${ethers.formatUnits(ubal, 6)} mUSDT and ${ethers.formatEther(vbal)} ETH`);
console.log(`deposits: ${out.map((d) => `#${d.id} block ${d.block}`).join(", ")}  (recipient not visible on Ethereum: checked)`);
console.log("NEXT: wait ~15 minutes after the last deposit, then: npm run fetch");
