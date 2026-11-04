#!/usr/bin/env node
// Runs every check a pull request has to pass, in the same order as CI, and prints a summary.
//
//   npm run verify          everything (needs the Rust toolchain; the first Rust run is slow)
//   npm run verify:fast     frontend checks only (types, lint, format, unit tests, build)
//
// Extra flags: --stop-on-fail  stop at the first failure instead of running every step.
import { spawn } from "node:child_process";

const args = new Set(process.argv.slice(2));
const fast = args.has("--fast");
const stopOnFail = args.has("--stop-on-fail");

const steps = [
  { name: "Versions match", cmd: "npm run --silent check:versions" },
  { name: "TypeScript types", cmd: "npm run --silent typecheck" },
  { name: "ESLint", cmd: "npm run --silent lint" },
  { name: "Prettier format", cmd: "npm run --silent format:check" },
  { name: "Frontend unit tests", cmd: "npm run --silent test" },
  { name: "Frontend build", cmd: "npm run --silent build" },
  { name: "Rust formatting", cmd: "npm run --silent rust:fmt", rust: true },
  { name: "Rust clippy (lint)", cmd: "npm run --silent rust:clippy", rust: true },
  { name: "Rust unit tests", cmd: "npm run --silent rust:test", rust: true },
];

const run = (cmd) =>
  new Promise((resolve) => {
    const child = spawn(cmd, { shell: true, stdio: "inherit" });
    child.on("close", (code) => resolve(code ?? 1));
    child.on("error", () => resolve(1));
  });

const results = [];
for (const step of steps) {
  if (fast && step.rust) {
    results.push({ ...step, status: "skipped", ms: 0 });
    continue;
  }
  console.log(`\n━━ ${step.name} ━━`);
  const t0 = Date.now();
  const code = await run(step.cmd);
  results.push({ ...step, status: code === 0 ? "passed" : "FAILED", ms: Date.now() - t0 });
  if (code !== 0 && stopOnFail) break;
}

console.log("\n━━ Summary ━━");
const icon = { passed: "✔", FAILED: "✘", skipped: "–" };
for (const r of results) {
  console.log(
    ` ${icon[r.status]} ${r.name.padEnd(24)} ${r.status.padEnd(8)} ${r.ms ? (r.ms / 1000).toFixed(1) + "s" : ""}`,
  );
}
const failed = results.filter((r) => r.status === "FAILED");
if (failed.length) {
  console.log(`\n${failed.length} step(s) failed. Fix them before opening a pull request.`);
  process.exit(1);
}
console.log(
  fast
    ? "\nFrontend checks passed. Run `npm run verify` for the full set before you open a PR."
    : "\nAll checks passed. You are ready to open a pull request.",
);
