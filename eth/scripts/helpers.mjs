import "dotenv/config";
import { ethers } from "ethers";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import solc from "solc";

export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
export const VECTORS = path.resolve(ROOT, "..", "vectors", process.env.NETWORK || "sepolia");

export function need(name) {
  const v = process.env[name];
  if (!v) {
    console.error(`Missing ${name} in eth/.env (see .env.example)`);
    process.exit(1);
  }
  return v;
}

export function provider() {
  return new ethers.JsonRpcProvider(need("EXEC_RPC"));
}

export function wallet() {
  return new ethers.Wallet(need("PRIVATE_KEY"), provider());
}

// `file` is relative to eth/contracts; @openzeppelin imports resolve from node_modules.
export function compile(file, contractName) {
  const input = {
    language: "Solidity",
    sources: { [file]: { content: fs.readFileSync(path.join(ROOT, "contracts", file), "utf8") } },
    settings: {
      optimizer: { enabled: true, runs: 200 },
      outputSelection: { "*": { "*": ["abi", "evm.bytecode.object", "storageLayout"] } },
    },
  };
  const findImports = (p) => {
    for (const full of [path.join(ROOT, "node_modules", p), path.join(ROOT, "contracts", p)])
      if (fs.existsSync(full)) return { contents: fs.readFileSync(full, "utf8") };
    return { error: "not found: " + p };
  };
  const out = JSON.parse(solc.compile(JSON.stringify(input), { import: findImports }));
  const errors = (out.errors || []).filter((e) => e.severity === "error");
  if (errors.length) throw new Error(errors.map((e) => e.formattedMessage).join("\n"));
  const c = out.contracts[file][contractName];
  return { abi: c.abi, bytecode: "0x" + c.evm.bytecode.object, storageLayout: c.storageLayout };
}

export function readJson(name) {
  const p = path.join(VECTORS, name);
  return fs.existsSync(p) ? JSON.parse(fs.readFileSync(p, "utf8")) : null;
}

export function writeJson(name, obj) {
  fs.mkdirSync(VECTORS, { recursive: true });
  const p = path.join(VECTORS, name);
  fs.writeFileSync(p, JSON.stringify(obj, (_, v) => (typeof v === "bigint" ? v.toString() : v), 2) + "\n");
  return p;
}
