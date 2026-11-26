// Sends AMOUNT tUSD straight to HOLDER. Only a fallback: this is not an escrow.
import { ethers } from "ethers";
import { need, wallet } from "./helpers.mjs";

const w = wallet();
const token = new ethers.Contract(
  need("TOKEN"),
  ["function transfer(address,uint256) returns (bool)", "function balanceOf(address) view returns (uint256)"],
  w
);
const holder = need("HOLDER");
const amount = ethers.parseUnits(process.env.AMOUNT || "1000", 6);
const tx = await token.transfer(holder, amount);
console.log(`transfer tx: ${tx.hash}`);
await tx.wait();
console.log(`HOLDER balance now: ${ethers.formatUnits(await token.balanceOf(holder), 6)} tUSD`);
