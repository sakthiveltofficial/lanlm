// Guards the repository's own plumbing: workflows must be valid and call real scripts,
// the version numbers must agree, and the community files must exist.
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { parse } from "yaml";

const root = fileURLToPath(new URL("..", import.meta.url));
const read = (p: string) => readFileSync(join(root, p), "utf8");
const pkg = JSON.parse(read("package.json")) as { scripts: Record<string, string>; version: string };

type Step = { uses?: string; run?: string; name?: string };
type Job = { "runs-on"?: string; needs?: string[] | string; steps?: Step[]; if?: string };
type Workflow = { name?: string; on?: unknown; jobs?: Record<string, Job> };

/** Width, height and colour type from a PNG's header (colour type 6 = RGBA). */
function pngInfo(buf: Buffer): { width: number; height: number; colorType: number } {
  expect(buf.subarray(1, 4).toString("ascii"), "not a PNG file").toBe("PNG");
  return { width: buf.readUInt32BE(16), height: buf.readUInt32BE(20), colorType: buf[25] };
}

/** Width and height of a JPEG, read from its start-of-frame marker. */
function jpgSize(buf: Buffer): { width: number; height: number } {
  let i = 2;
  while (i < buf.length) {
    if (buf[i] !== 0xff) throw new Error("bad JPEG marker");
    const marker = buf[i + 1];
    if (marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc) {
      return { height: buf.readUInt16BE(i + 5), width: buf.readUInt16BE(i + 7) };
    }
    i += 2 + buf.readUInt16BE(i + 2);
  }
  throw new Error("no JPEG size found");
}

const workflowDir = join(root, ".github", "workflows");
const workflows = readdirSync(workflowDir)
  .filter((f) => f.endsWith(".yml"))
  .map((f) => ({ file: f, doc: parse(readFileSync(join(workflowDir, f), "utf8")) as Workflow }));

describe("GitHub workflows", () => {
  it("includes CI, release and CodeQL", () => {
    const names = workflows.map((w) => w.file);
    expect(names).toEqual(expect.arrayContaining(["ci.yml", "release.yml", "codeql.yml"]));
  });

  it.each(workflows)("$file is well formed", ({ doc }) => {
    expect(doc.name).toBeTruthy();
    expect(doc.on).toBeTruthy();
    const jobs = Object.values(doc.jobs ?? {});
    expect(jobs.length).toBeGreaterThan(0);
    for (const job of jobs) {
      expect(job["runs-on"]).toBeTruthy();
      expect(job.steps?.length ?? 0).toBeGreaterThan(0);
    }
  });

  it("pins every action to a version", () => {
    for (const { file, doc } of workflows) {
      for (const job of Object.values(doc.jobs ?? {})) {
        for (const step of job.steps ?? []) {
          if (step.uses) expect(step.uses, `${file}: ${step.uses}`).toMatch(/@[\w.-]+$/);
        }
      }
    }
  });

  it("only calls npm scripts that exist", () => {
    for (const { file, doc } of workflows) {
      for (const job of Object.values(doc.jobs ?? {})) {
        for (const step of job.steps ?? []) {
          for (const m of (step.run ?? "").matchAll(/npm run ([\w:-]+)/g)) {
            expect(pkg.scripts, `${file} runs "npm run ${m[1]}"`).toHaveProperty(m[1]);
          }
        }
      }
    }
  });

  it("publishes the Windows installer, a portable exe and checksums on a tag", () => {
    const release = readFileSync(join(workflowDir, "release.yml"), "utf8");
    const doc = parse(release) as Workflow & { on: { push: { tags: string[] } } };
    expect(doc.on.push.tags).toContain("v*");
    expect(release).toContain("tauri-apps/tauri-action");
    expect(release).toContain("releaseDraft: false");
    expect(release).toContain("_x64_portable.exe");
    expect(release).toContain("SHA256SUMS.txt");
    expect(release).toContain("gh release upload");
    expect(release).toContain("check-version.mjs");
    // The release must run the tests before it ships anything.
    expect(release.indexOf("cargo test")).toBeGreaterThan(-1);
    expect(release.indexOf("cargo test")).toBeLessThan(release.indexOf("tauri-apps/tauri-action"));
  });

  it("makes the CI success job wait for every required job", () => {
    const ci = workflows.find((w) => w.file === "ci.yml")!.doc.jobs!;
    const needs = ci["ci-success"].needs as string[];
    for (const required of ["frontend", "rust", "app-build"]) expect(needs).toContain(required);
    expect(ci["ci-success"].if).toContain("always()");
  });
});

describe("repository files", () => {
  it.each([
    "README.md",
    "LICENSE",
    "CONTRIBUTING.md",
    "CODE_OF_CONDUCT.md",
    "SECURITY.md",
    "CHANGELOG.md",
    "docs/ARCHITECTURE.md",
    "docs/PUBLISHING.md",
    ".github/PULL_REQUEST_TEMPLATE.md",
    ".github/dependabot.yml",
    ".github/ISSUE_TEMPLATE/bug_report.yml",
    ".github/ISSUE_TEMPLATE/feature_request.yml",
    ".github/ISSUE_TEMPLATE/config.yml",
  ])("%s exists", (file) => {
    expect(existsSync(join(root, file))).toBe(true);
  });

  it("README images all exist", () => {
    const images = [...read("README.md").matchAll(/src="(docs\/images\/[^"]+)"/g)].map((m) => m[1]);
    // The README uses the banner only; app screenshots are deliberately not kept in the repository.
    expect(images).toEqual(["docs/images/banner.png"]);
    for (const img of images) expect(existsSync(join(root, img)), img).toBe(true);
  });

  it("README links to files that exist", () => {
    const links = [...read("README.md").matchAll(/\]\(((?:docs\/|\.\/)?[A-Za-z_]+(?:\/[\w.-]+)*\.md)(?:#[\w-]+)?\)/g)].map((m) => m[1]);
    expect(links.length).toBeGreaterThan(3);
    for (const link of links) expect(existsSync(join(root, link)), link).toBe(true);
  });

  it("the app icon is a square PNG with transparency", () => {
    const info = pngInfo(readFileSync(join(root, "app-icon.png")));
    expect(info.width).toBe(info.height);
    expect(info.width).toBeGreaterThanOrEqual(512);
    // Colour type 6 is RGBA: the rounded corners must be transparent, not black.
    expect(info.colorType, "app-icon.png needs an alpha channel").toBe(6);
  });

  it("the generated Windows icons exist and the largest is square", () => {
    for (const f of ["32x32.png", "128x128.png", "128x128@2x.png", "icon.ico", "icon.png"]) {
      expect(existsSync(join(root, "src-tauri", "icons", f)), f).toBe(true);
    }
    const png = pngInfo(readFileSync(join(root, "src-tauri", "icons", "icon.png")));
    expect(png.width).toBe(png.height);
    const tauriConf = JSON.parse(read("src-tauri/tauri.conf.json")) as { bundle: { icon: string[] } };
    for (const icon of tauriConf.bundle.icon) expect(existsSync(join(root, "src-tauri", icon)), icon).toBe(true);
  });

  it("the banner is 2:1 and the social preview is 1280x640 and under GitHub's 1 MB limit", () => {
    const banner = pngInfo(readFileSync(join(root, "docs/images/banner.png")));
    expect(banner.width / banner.height).toBeCloseTo(2, 2);
    const jpg = readFileSync(join(root, "docs/images/social-preview.jpg"));
    expect(jpgSize(jpg)).toEqual({ width: 1280, height: 640 });
    expect(jpg.length).toBeLessThan(1_000_000);
  });

  it("the changelog has an entry for the current version", () => {
    expect(read("CHANGELOG.md")).toContain(`## [${pkg.version}]`);
  });

  it("is MIT licensed in every manifest", () => {
    expect(read("LICENSE")).toContain("MIT License");
    expect(read("src-tauri/Cargo.toml")).toMatch(/license\s*=\s*"MIT"/);
    expect((JSON.parse(read("package.json")) as { license: string }).license).toBe("MIT");
  });
});

describe("secrets and local data", () => {
  it("does not ship a local API key or personal paths in the docs", () => {
    for (const file of ["README.md", "CONTRIBUTING.md", "docs/ARCHITECTURE.md", "docs/PUBLISHING.md"]) {
      const text = read(file);
      expect(text, file).not.toMatch(/lanlm-[0-9a-f]{20,}/);
      expect(text, file).not.toMatch(/C:\\Users\\[A-Za-z]/);
    }
  });
});
