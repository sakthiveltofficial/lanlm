#!/usr/bin/env node
// Points the project at a GitHub repository: README badges and links, package.json,
// Cargo.toml, issue templates, workflows and docs. It replaces whatever address the
// project currently uses (initially the OWNER/REPO placeholder), so it can be re-run
// if you rename or move the repository.
//
//   npm run set-repo -- your-username/lanlm
import { readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const target = process.argv[2];
if (!target || !/^[\w.-]+\/[\w.-]+$/.test(target)) {
  console.error("Usage: npm run set-repo -- <github-user-or-org>/<repository>   e.g. octocat/lanlm");
  process.exit(1);
}

const root = fileURLToPath(new URL("..", import.meta.url));

// The address in use now is the one recorded in package.json.
const pkgUrl = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).repository?.url ?? "";
const current = /github\.com\/([\w.-]+\/[\w.-]+?)(?:\.git)?$/.exec(pkgUrl)?.[1];
if (!current) {
  console.error("Could not read the current repository address from package.json (repository.url).");
  process.exit(1);
}
if (current === target) {
  console.log(`Already set to ${target}. Nothing to change.`);
  process.exit(0);
}

const skip = new Set(["node_modules", "target", "dist", ".git", "gen"]);
const exts = [".md", ".json", ".toml", ".yml", ".yaml"];

let changed = 0;
const walk = (dir) => {
  for (const name of readdirSync(dir)) {
    if (skip.has(name)) continue;
    const path = join(dir, name);
    if (statSync(path).isDirectory()) {
      walk(path);
    } else if (exts.some((e) => name.endsWith(e)) && name !== "package-lock.json" && name !== "Cargo.lock") {
      const before = readFileSync(path, "utf8");
      // Only replace complete GitHub addresses, never the bare words in prose or code.
      const after = before
        .replaceAll(`github.com/${current}`, `github.com/${target}`)
        .replaceAll(`shields.io/github/v/release/${current}`, `shields.io/github/v/release/${target}`);
      if (after !== before) {
        writeFileSync(path, after);
        console.log("updated", relative(root, path));
        changed++;
      }
    }
  }
};
walk(root);
console.log(
  changed
    ? `\nDone: ${changed} file(s) now point to https://github.com/${target} (was ${current})`
    : "Nothing to change.",
);
