// Deploys DESCROW, creates a one-milestone deal in tUSD and funds it through
// fundMilestone(0). Re-runnable: progress (incl. the test seller key) is kept in
// eth/.descrow-state.json, which is gitignored.
import { ethers } from "ethers";
import fs from "node:fs";
import path from "node:path";
import { compile, need, wallet, writeJson, ROOT } from "./helpers.mjs";

const STATE = path.join(ROOT, ".descrow-state.json");
const state = fs.existsSync(STATE) ? JSON.parse(fs.readFileSync(STATE, "utf8")) : {};
const save = () => fs.writeFileSync(STATE, JSON.stringify(state, null, 2));

const baseBuyer = wallet();
const p = baseBuyer.provider;
const buyer = new ethers.NonceManager(baseBuyer);
buyer.address = baseBuyer.address;
const TOKEN = ethers.getAddress(need("TOKEN"));
const AMOUNT = ethers.parseUnits(process.env.DEAL_AMOUNT || "1000", 6);
const chainId = (await p.getNetwork()).chainId;
console.log(`chain ${chainId}  buyer ${buyer.address}  token ${TOKEN}`);

const token = new ethers.Contract(TOKEN, [
  "function balanceOf(address) view returns (uint256)",
  "function approve(address,uint256) returns (bool)",
  "function allowance(address,address) view returns (uint256)",
], buyer);
if ((await token.balanceOf(buyer.address)) < AMOUNT) throw new Error("Buyer has less tUSD than DEAL_AMOUNT");

// test seller, platform wallet and arbitrators
if (!state.sellerKey) { state.sellerKey = ethers.Wallet.createRandom().privateKey; save(); }
const baseSeller = new ethers.Wallet(state.sellerKey, p);
const seller = new ethers.NonceManager(baseSeller);
seller.address = baseSeller.address;
state.platformWallet ??= ethers.Wallet.createRandom().address;
state.arbitrators ??= [0, 1, 2].map(() => ethers.Wallet.createRandom().address);
save();
console.log(`seller ${seller.address} (test wallet, key in .descrow-state.json)`);

if ((await p.getBalance(seller.address)) < ethers.parseEther("0.002")) {
  const tx = await buyer.sendTransaction({ to: seller.address, value: ethers.parseEther("0.005") });
  console.log(`funding seller gas: ${tx.hash}`);
  await tx.wait();
}

const F = compile("descrow/EscrowFactory.sol", "EscrowFactory");
const A = compile("descrow/EscrowAgreement.sol", "EscrowAgreement");
let factory;
if (state.factory) {
  factory = new ethers.Contract(state.factory, F.abi, buyer);
  console.log(`factory ${state.factory} (reused)`);
} else {
  const tx = await new ethers.ContractFactory(F.abi, F.bytecode, buyer).deploy(state.platformWallet);
  console.log(`deploying EscrowFactory: ${tx.deploymentTransaction().hash}`);
  await tx.waitForDeployment();
  state.factory = await tx.getAddress(); save();
  factory = tx;
  console.log(`factory ${state.factory}`);
}

if (!state.deal) {
  const termsHash = "sha256:" + ethers.sha256(ethers.toUtf8Bytes("CFT x Logos RFP-021 PoC - milestone 1: deliver attestation demo")).slice(2);
  const milestone = { amount: AMOUNT, termsHash, defaultSellerBps: 10000, reviewPeriod: 3 * 86400, completionDeadline: 30 * 86400, gracePeriod: 0 };
  const tx = await factory.createEscrowAgreement(buyer.address, seller.address, TOKEN, state.arbitrators, [milestone]);
  console.log(`createEscrowAgreement: ${tx.hash}`);
  const rc = await tx.wait();
  const ev = rc.logs.map((l) => { try { return factory.interface.parseLog(l); } catch { return null; } }).find((e) => e?.name === "AgreementDeployed");
  state.deal = ev.args.agreement; state.dealCreatedBlock = rc.blockNumber; save();
}
const deal = new ethers.Contract(state.deal, A.abi, buyer);
console.log(`deal ${state.deal}`);

if (!(await deal.sellerAcknowledged())) {
  const tx = await deal.connect(seller).acknowledgeAgreement();
  console.log(`seller acknowledgeAgreement: ${tx.hash}`);
  await tx.wait();
}
if (!(await deal.dealActive())) throw new Error("deal not active after acknowledgement");

let m = await deal.milestones(0);
if (Number(m.status) === 0) {
  if ((await token.allowance(buyer.address, state.deal)) < AMOUNT) {
    const a = await token.approve(state.deal, AMOUNT);
    console.log(`approve: ${a.hash}`);
    await a.wait();
  }
  const tx = await deal.fundMilestone(0);
  console.log(`fundMilestone(0): ${tx.hash}`);
  const rc = await tx.wait();
  state.fundedBlock = rc.blockNumber; state.fundTx = tx.hash; save();
  m = await deal.milestones(0);
}

// slots we prove, checked against the contract's own getters
const layout = A.storageLayout;
const msVar = layout.storage.find((s) => s.label === "milestones");
const msType = Object.values(layout.types).find((t) => t.label === "struct EscrowAgreement.Milestone" && t.members);
const off = (label) => BigInt(msType.members.find((x) => x.label === label).slot);
const elemSlots = BigInt(msType.numberOfBytes) / 32n;
const base = BigInt(ethers.keccak256(ethers.toBeHex(BigInt(msVar.slot), 32)));
const slotOf = (i, label) => ethers.toBeHex(base + BigInt(i) * elemSlots + off(label), 32);
const slots = { escrowedAmount: slotOf(0, "escrowedAmount"), status: slotOf(0, "status") };

const onchainEscrowed = BigInt(await p.getStorage(state.deal, slots.escrowedAmount));
const onchainStatus = BigInt(await p.getStorage(state.deal, slots.status));
if (onchainEscrowed !== m.escrowedAmount || onchainStatus !== BigInt(m.status))
  throw new Error(`storage layout check failed: slot values ${onchainEscrowed}/${onchainStatus} vs getter ${m.escrowedAmount}/${m.status}`);
const dealBalance = await token.balanceOf(state.deal);

writeJson("descrow.json", {
  descrow_version: "v2.2 (unmodified)",
  chain_id: chainId, factory: state.factory, deal: state.deal, buyer: buyer.address, seller: seller.address,
  token: TOKEN, platform_wallet: state.platformWallet, arbitrators: state.arbitrators,
  milestone_id: 0, funded_block: state.fundedBlock, fund_tx: state.fundTx,
  milestones_slot: msVar.slot, milestone_struct_slots: Number(elemSlots),
  slots, expected: { escrowedAmount: m.escrowedAmount, status: Number(m.status), status_name: "Funded", deal_token_balance: dealBalance },
});

console.log(`\nDESCROW deal ${state.deal}`);
console.log(`  milestone 0 status   ${m.status} (1 = Funded)`);
console.log(`  escrowedAmount       ${ethers.formatUnits(m.escrowedAmount, 6)} tUSD (after the 2% platform fee)`);
console.log(`  deal's tUSD balance  ${ethers.formatUnits(dealBalance, 6)} tUSD`);
console.log(`  storage slots checked: escrowedAmount ${slots.escrowedAmount}, status ${slots.status}`);
console.log(`  funded in block ${state.fundedBlock}`);
console.log(`\nNEXT: set HOLDER=${state.deal} in eth/.env, wait ~15 minutes, then: npm run fetch`);
