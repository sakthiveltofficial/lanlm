// Static checks over the UI source. They catch the mistakes a hand-written
// template UI is prone to: a button whose handler was never written, or an
// icon name that does not exist.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { ICONS } from "./lib/icons";

const main = readFileSync(new URL("./main.ts", import.meta.url), "utf8");
const meshSource = readFileSync(new URL("./mesh.ts", import.meta.url), "utf8");
// The cluster screen lives in its own file; together they are "the UI".
const source = main + "\n" + meshSource;

describe("main.ts consistency", () => {
  it("has a click handler for every data-act used in a template", () => {
    const used = new Set([...source.matchAll(/data-act="([a-z0-9-]+)"/g)].map((m) => m[1]));
    expect(used.size).toBeGreaterThan(20);
    const missing = [...used].filter((name) => !source.includes(`case "${name}"`));
    expect(missing).toEqual([]);
  });

  it("only uses icons that exist", () => {
    const names = new Set([...source.matchAll(/\bic\("([a-z-]+)"\)/g)].map((m) => m[1]));
    expect(names.size).toBeGreaterThan(10);
    const unknown = [...names].filter((n) => !(n in ICONS));
    expect(unknown).toEqual([]);
  });

  it("only navigates to views that exist", () => {
    const views = new Set([...source.matchAll(/data-view="([a-z]+)"/g)].map((m) => m[1]));
    expect([...views].filter((v) => !["chat", "models", "cluster"].includes(v))).toEqual([]);
  });

  it("never renders model output without escaping it", () => {
    // Model text must go through md()/esc(). A raw `${m.content}` on a line that also
    // builds HTML would be an XSS hole (plain-text uses, such as the prompt, are fine).
    const risky = source.split("\n").filter((line) => /<[a-z][^>]*>/i.test(line) && /\$\{[^}]*\.(content|reasoning)\}/.test(line));
    expect(risky).toEqual([]);
  });
});

describe("mesh.ts", () => {
  it("escapes everything another device can influence", () => {
    // Names, file names, errors and messages come from other devices over the network, so each
    // placeholder that uses one must go through esc() (or a helper that escapes) in HTML.
    const fields = ["r.name", "n.name", "m.file", "t.file", "i.error", "ds?.message", "n.record.name", "inst.model", "d.model", "s.name", "b.nodeName", "r.file"];
    // dis() and comparisons produce fixed words, never the field itself.
    const safe = /esc\(|badge\(|short\(|modelLabel\(|bytesText\(|gb\(|ago\(|dis\(| === |\.length|\.map\(/;
    const bad: string[] = [];
    for (const line of meshSource.split("\n").filter((l) => /<[a-z]/i.test(l))) {
      for (const m of line.matchAll(/\$\{([^}]*)\}/g)) {
        const expr = m[1];
        if (fields.some((f) => expr.includes(f)) && !safe.test(expr)) bad.push(expr);
      }
    }
    expect(bad).toEqual([]);
  });

  it("handles every control it draws", () => {
    const acts = new Set([...meshSource.matchAll(/data-act="(mesh-[a-z-]+)"/g)].map((m) => m[1]));
    expect(acts.size).toBeGreaterThan(20);
    const handled = new Set([...meshSource.matchAll(/case "(mesh-[a-z-]+)"/g)].map((m) => m[1]));
    expect([...acts].filter((a) => !handled.has(a))).toEqual([]);
  });

  it("uses only commands the desktop backend defines", () => {
    const lib = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
    const used = new Set([...meshSource.matchAll(/invoke(?:<[^>]*>)?\("(mesh_[a-z_]+)"/g)].map((m) => m[1]));
    expect(used.size).toBeGreaterThan(15);
    const missing = [...used].filter((c) => !lib.includes(`mesh::${c}`));
    expect(missing).toEqual([]);
  });
});
