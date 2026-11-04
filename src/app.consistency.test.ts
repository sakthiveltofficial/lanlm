// Static checks over the UI source. They catch the mistakes a hand-written
// template UI is prone to: a button whose handler was never written, or an
// icon name that does not exist.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { ICONS } from "./lib/icons";

const source = readFileSync(new URL("./main.ts", import.meta.url), "utf8");

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
