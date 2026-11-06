import { describe, expect, it } from "vitest";
import {
  activity,
  ago,
  allInstances,
  bytesText,
  deviceState,
  formatCode,
  gatewayExample,
  gpuLine,
  layerCells,
  makeKey,
  modelMatrix,
  parseAddress,
  planSummary,
  totals,
  usedPct,
  type NodeRecord,
  type NodeView,
  type PlanResult,
  type Stage,
} from "./mesh";

const rec = (over: Partial<NodeRecord> = {}): NodeRecord => ({
  nodeId: "a",
  name: "Alpha",
  clusterId: "c",
  version: "1",
  os: "windows",
  addrs: [],
  hw: { cpu: "cpu", cores: 8, ramTotalMb: 16384, ramFreeMb: 8192, gpus: [], memBwGbps: 40 },
  engine: { installed: true, version: "b1", variant: "cpu", hasRpc: true },
  models: [],
  transfers: [],
  tensorCacheMb: 0,
  lend: true,
  draining: false,
  instance: null,
  share: null,
  deployments: [],
  bench: null,
  gatewayInflight: 0,
  gatewayPort: 0,
  ...over,
});
const view = (over: Partial<NodeRecord> = {}, v: Partial<NodeView> = {}): NodeView => ({
  record: rec(over),
  alive: true,
  linked: true,
  isMe: false,
  rttMs: 1,
  ageMs: 0,
  ...v,
});
const inst = (over = {}) => ({
  id: "i",
  deployment: "d",
  model: "m.gguf",
  kind: "replica",
  master: "a",
  stages: [],
  state: "ready" as const,
  error: "",
  ctx: 4096,
  slots: 4,
  busy: 1,
  queued: 0,
  requestsTotal: 0,
  tokS: 25.4,
  startedMs: 0,
  ...over,
});

describe("pairing and addresses", () => {
  it("groups the six digits for reading aloud", () => {
    expect(formatCode("483920")).toBe("483 920");
    expect(formatCode("123")).toBe("123");
  });
  it("accepts hosts with or without a port", () => {
    expect(parseAddress("192.168.1.20")).toEqual({ addr: "192.168.1.20:50500" });
    expect(parseAddress(" pc-name:50511 ")).toEqual({ addr: "pc-name:50511" });
    expect("error" in parseAddress("not an address!")).toBe(true);
    expect("error" in parseAddress("host:99999")).toBe(true);
    expect("error" in parseAddress("")).toBe(true);
  });
});

describe("device summaries", () => {
  it("says how long ago in words", () => {
    expect(ago(1000)).toBe("just now");
    expect(ago(30_000)).toBe("30 s ago");
    expect(ago(5 * 60_000)).toBe("5 min ago");
    expect(ago(3 * 3_600_000)).toBe("3 h ago");
  });
  it("names the state of a device", () => {
    expect(deviceState(view({}, { isMe: true })).text).toBe("this PC");
    expect(deviceState(view()).text).toBe("online");
    expect(deviceState(view({}, { alive: false })).cls).toBe("bad");
    expect(deviceState(view({ draining: true })).text).toBe("being emptied");
    expect(deviceState(view({ lend: false })).text).toBe("not lending");
    expect(deviceState(view({ instance: inst({ state: "failed" }) })).text).toBe("model failed");
  });
  it("describes what a device is doing", () => {
    expect(activity(rec())).toBe("Idle — ready for work");
    expect(activity(rec({ instance: inst() }))).toContain("1/4 slots busy · 25 tok/s");
    expect(
      activity(rec({ instance: inst({ state: "loading", kind: "pipeline", stages: [{} as Stage, {} as Stage] }) })),
    ).toContain("split over 2 PCs");
    expect(
      activity(
        rec({
          share: { instance: "i", model: "big.gguf", master: "b", layerFrom: 10, layers: 5, memMb: 1, running: true },
        }),
      ),
    ).toBe("Holding layers 11–15 of big.gguf");
    expect(
      activity(rec({ transfers: [{ file: "x.gguf", done: 1, total: 2, from: [], chunkSize: 1, have: "" }] })),
    ).toBe("Copying x.gguf");
  });
  it("shortens GPU names and handles none", () => {
    expect(gpuLine(rec().hw)).toBe("");
    const hw = {
      ...rec().hw,
      gpus: [
        { device: "CUDA0", name: "NVIDIA GeForce RTX 5060 Ti", backend: "cuda", vramTotalMb: 16311, vramFreeMb: 15000 },
      ],
    };
    expect(gpuLine(hw)).toBe("RTX 5060 Ti · 16 GB");
  });
  it("computes memory use safely", () => {
    expect(usedPct(100, 25)).toBe(75);
    expect(usedPct(0, 0)).toBe(0);
    expect(usedPct(100, 150)).toBe(0);
  });
});

describe("cluster totals", () => {
  it("adds up online devices only", () => {
    const nodes = [
      view({ instance: inst({ tokS: 20, busy: 2 }) }),
      view({ nodeId: "b", instance: inst({ tokS: 30, busy: 1, slots: 2 }) }),
      view({ nodeId: "c", instance: inst({ tokS: 99 }) }, { alive: false }),
    ];
    const t = totals(nodes);
    expect(t).toMatchObject({ online: 2, devices: 3, copies: 2, tokS: 50, busy: 3, slots: 6 });
    expect(t.ramGb).toBeCloseTo(32);
  });
  it("does not count a model that is still loading as a running copy", () => {
    expect(totals([view({ instance: inst({ state: "loading" }) })]).copies).toBe(0);
  });
  it("lists the running instances with their device", () => {
    const list = allInstances([view({ instance: inst() }), view({ nodeId: "b" })]);
    expect(list.map((x) => x.node.name)).toEqual(["Alpha"]);
  });
});

describe("model matrix", () => {
  it("shows which devices have, are copying or lack each model", () => {
    const m = { file: "m.gguf", size: 4e9, sha256: "a".repeat(64), nLayer: 32 };
    const nodes = [
      view({ nodeId: "a", models: [m] }),
      view({ nodeId: "b", transfers: [{ file: "m.gguf", done: 1, total: 4, from: [], chunkSize: 1, have: "" }] }),
      view({ nodeId: "c" }),
      view({ nodeId: "d", models: [m] }, { alive: false }),
    ];
    const rows = modelMatrix(nodes);
    expect(rows).toHaveLength(1);
    expect(rows[0].nLayer).toBe(32);
    expect(rows[0].cells.map((c) => c.nodeId)).toEqual(["a", "b", "c"]); // offline devices are left out
    expect(rows[0].cells[0]).toMatchObject({ has: true, hashOk: true });
    expect(rows[0].cells[1]).toMatchObject({ has: false, copying: 25 });
    expect(rows[0].cells[2]).toMatchObject({ has: false, copying: null });
  });
  it("sorts by name and lists a model that is only being copied", () => {
    const nodes = [
      view({
        nodeId: "a",
        models: [{ file: "b.gguf", size: 1, sha256: "", nLayer: 1 }],
        transfers: [{ file: "a.gguf", done: 0, total: 9, from: [], chunkSize: 1, have: "" }],
      }),
    ];
    const rows = modelMatrix(nodes);
    expect(rows.map((r) => r.file)).toEqual(["a.gguf", "b.gguf"]);
    expect(rows[0].anyone).toBe(false);
  });
});

describe("layer picture and plans", () => {
  const stages: Stage[] = [
    { nodeId: "a", name: "A", layerFrom: 0, layers: 3, memMb: 1, device: "gpu", isMaster: true },
    { nodeId: "b", name: "B", layerFrom: 3, layers: 2, memMb: 1, device: "cpu", isMaster: false },
  ];
  it("gives each layer to its device", () => {
    expect(layerCells(stages, 5).map((c) => c.device)).toEqual([0, 0, 0, 1, 1]);
    expect(layerCells(stages, 5)[3].layer).toBe(4);
  });
  it("marks layers nobody holds", () => {
    expect(
      layerCells(stages, 7)
        .slice(5)
        .map((c) => c.device),
    ).toEqual([-1, -1]);
  });
  it("summarises a plan in a sentence", () => {
    const p = (kind: string, tok: number) => ({
      kind,
      master: "a",
      stages: [],
      ngl: null,
      tensorSplit: [],
      rpcNodes: [],
      pinOutput: "",
      needsFetch: false,
      estTokS: tok,
      note: "",
    });
    const plan: PlanResult = {
      instances: [p("replica", 30), p("replica", 20), p("pipeline", 5)],
      notes: [],
      error: null,
      needMb: 1,
    };
    expect(planSummary(plan)).toBe("2 full copies and 1 split across devices · about 55 tokens/s in total");
    expect(planSummary({ instances: [], notes: [], error: "Too big", needMb: 1 })).toBe("Too big");
    expect(planSummary(null)).toBe("");
  });
});

describe("gateway helpers", () => {
  it("builds a copyable example with or without a key", () => {
    expect(gatewayExample("http://x:8080/v1", "")).not.toContain("Authorization");
    expect(gatewayExample("http://x:8080/v1", "k1")).toContain("Authorization: Bearer k1");
    expect(gatewayExample("http://x:8080/v1", "")).toContain("http://x:8080/v1/chat/completions");
  });
  it("makes keys from bytes", () => {
    expect(makeKey(new Uint8Array([0, 15, 255]))).toBe("lanlm-000fff");
  });
  it("prints sizes", () => {
    expect(bytesText(4_100_000_000)).toBe("4.1 GB");
    expect(bytesText(820_000_000)).toBe("820 MB");
  });
});
