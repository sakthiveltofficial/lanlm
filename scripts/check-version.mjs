#!/usr/bin/env node
// Makes sure the three version numbers agree (package.json, Cargo.toml, tauri.conf.json),
// and, when given a tag such as v1.2.3, that the tag matches them. Used by CI and the release workflow.
import { readFileSync } from "node:fs";

const read = (p) => readFileSync(new URL(`../${p}`, import.meta.url), "utf8");
const pkg = JSON.parse(read("package.json")).version;
const tauri = JSON.parse(read("src-tauri/tauri.conf.json")).version;
const cargo = /^version\s*=\s*"([^"]+)"/m.exec(read("src-tauri/Cargo.toml"))?.[1];

const versions = { "package.json": pkg, "src-tauri/Cargo.toml": cargo, "src-tauri/tauri.conf.json": tauri };
const tag = process.argv[2]?.replace(/^v/, "");
if (tag) versions["git tag"] = tag;

const unique = new Set(Object.values(versions));
if (unique.size !== 1 || unique.has(undefined)) {
  console.error("Version mismatch:");
  for (const [file, v] of Object.entries(versions)) console.error(`  ${file.padEnd(28)} ${v ?? "(not found)"}`);
  process.exit(1);
}
console.log(`Versions agree: ${pkg}`);
