// Deploys TestUSD and mints 1,000,000 tUSD to the deployer.
import { ethers } from "ethers";
import { compile, wallet } from "./helpers.mjs";

const w = wallet();
const net = await w.provider.getNetwork();
console.log(`Network chainId=${net.chainId}  deployer=${w.address}`);
console.log(`Balance: ${ethers.formatEther(await w.provider.getBalance(w.address))} ETH`);

const { abi, bytecode } = compile("TestUSD.sol", "TestUSD");
const factory = new ethers.ContractFactory(abi, bytecode, w);
const token = await factory.deploy(ethers.parseUnits("1000000", 6));
console.log(`Deploy tx: ${token.deploymentTransaction().hash}`);
await token.waitForDeployment();
const addr = await token.getAddress();

console.log(`\nTestUSD deployed at ${addr}`);
console.log(`Add this line to eth/.env:\nTOKEN=${addr}\n`);
console.log("Next: create a DESCROW deal with TestUSD as paymentToken and fund it,");
console.log("then put the deal (clone) address in eth/.env as HOLDER=0x...");
