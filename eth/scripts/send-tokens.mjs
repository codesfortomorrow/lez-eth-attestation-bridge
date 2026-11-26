// Sends tUSD from your wallet to any address. A testing helper, not part of the escrow flow.
import { ethers } from "ethers";
import { need, wallet } from "./helpers.mjs";

const w = wallet();
const token = new ethers.Contract(
  need("TOKEN"),
  ["function transfer(address,uint256) returns (bool)", "function balanceOf(address) view returns (uint256)"],
  w
);
const to = ethers.getAddress(need("SEND_TO"));
const amount = ethers.parseUnits(process.env.AMOUNT || "1000", 6);
const tx = await token.transfer(to, amount);
console.log(`transfer tx: ${tx.hash}`);
await tx.wait();
console.log(`${to} now holds ${ethers.formatUnits(await token.balanceOf(to), 6)} tUSD`);