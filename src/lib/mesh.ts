// Pure helpers for the cluster screen: turning what the devices publish into rows and sentences.
// No DOM here, so all of it is unit tested.

export type Gpu = { device: string; name: string; backend: string; vramTotalMb: number; vramFreeMb: number };
export type Hw = { cpu: string; cores: number; ramTotalMb: number; ramFreeMb: number; gpus: Gpu[]; memBwGbps: number };
export type HeldModel = { file: string; size: number; sha256: string; nLayer: number };
export type Transfer = { file: string; done: number; total: number; from: string[]; chunkSize: number; have: string };
export type Stage = {
  nodeId: string;
  name: string;
  layerFrom: number;
  layers: number;
  memMb: number;
  device: string;
  isMaster: boolean;
};
export type InstanceState = "loading" | "ready" | "failed" | "stopping";
export type Instance = {
  id: string;
  deployment: string;
  model: string;
  kind: string;
  master: string;
  stages: Stage[];
  state: InstanceState;
  error: string;
  ctx: number;
  slots: number;
  busy: number;
  queued: number;
  requestsTotal: number;
  tokS: number;
  startedMs: number;
};
export type Share = {
  instance: string;
  model: string;
  master: string;
  layerFrom: number;
  layers: number;
  memMb: number;
  running: boolean;
};
export type Deployment = {
  id: string;
  model: string;
  mode: string;
  ctx: number;
  parallel: number;
  nodes: string[];
  maxInstances: number;
  url: string;
  sha256: string;
};
export type NodeRecord = {
  nodeId: string;
  name: string;
  clusterId: string;
  version: string;
  os: string;
  addrs: string[];
  hw: Hw;
  engine: { installed: boolean; version: string; variant: string; hasRpc: boolean };
  models: HeldModel[];
  transfers: Transfer[];
  tensorCacheMb: number;
  lend: boolean;
  draining: boolean;
  instance: Instance | null;
  share: Share | null;
  deployments: Deployment[];
  bench: { tokS: number; model: string; modelGb: number; atMs: number } | null;
  gatewayInflight: number;
  gatewayPort: number;
};
export type NodeView = {
  record: NodeRecord;
  alive: boolean;
  linked: boolean;
  isMe: boolean;
  rttMs: number | null;
  ageMs: number;
};
export type PairStatus =
  | { state: "idle" }
  | { state: "connecting" }
  | { state: "waiting"; sas: string }
  | { state: "joined" }
  | { state: "denied" }
  | { state: "failed"; error: string };
export type Nearby = { nodeId: string; name: string; addrs: string[] };
export type PairRequest = { id: number; name: string; remoteId: string; sas: string; ageMs: number };
export type Snapshot = {
  me: string;
  name: string;
  joined: boolean;
  clusterId: string;
  clusterName: string;
  port: number;
  addrs: string[];
  nodes: NodeView[];
  pairingOpen: boolean;
  nearby: Nearby[];
  pairRequests: PairRequest[];
  pairStatus: PairStatus;
};
export type Backend = {
  instance: string;
  nodeId: string;
  nodeName: string;
  model: string;
  slots: number;
  busy: number;
  tokS: number;
};
export type GatewayStatus = {
  running: boolean;
  url: string;
  backends: Backend[];
  inflight: number;
  queued: number;
  total: number;
  failed: number;
  retried: number;
  refused: number;
};
export type GatewaySettings = { enabled: boolean; port: number; network: boolean; apiKeys: string[] };
export type DeployStatus = { message: string; isError: boolean; updatedMs: number };
export type MeshStatus = {
  mesh: Snapshot;
  deployments: Deployment[];
  deployStatus: Record<string, DeployStatus>;
  gateway: GatewayStatus | null;
  gatewaySettings: GatewaySettings;
  engineDir: string;
  modelsDir: string;
};
export type Planned = {
  kind: string;
  master: string;
  stages: Stage[];
  ngl: number | null;
  tensorSplit: number[];
  rpcNodes: string[];
  pinOutput: string;
  needsFetch: boolean;
  estTokS: number;
  note: string;
};
export type PlanResult = { instances: Planned[]; notes: string[]; error: string | null; needMb: number };

/** Six digits shown as "483 920" so two people can compare them quickly. */
export const formatCode = (sas: string): string => (sas.length === 6 ? `${sas.slice(0, 3)} ${sas.slice(3)}` : sas);

/** How long ago something was, in words. */
export function ago(ms: number): string {
  if (ms < 5_000) return "just now";
  if (ms < 60_000) return `${Math.round(ms / 1000)} s ago`;
  if (ms < 3_600_000) return `${Math.round(ms / 60_000)} min ago`;
  return `${Math.round(ms / 3_600_000)} h ago`;
}

/** A device's state as a word and a colour class. */
export function deviceState(v: NodeView): { text: string; cls: "good" | "warn" | "bad" | "" } {
  if (!v.alive) return { text: "offline", cls: "bad" };
  if (v.record.instance?.state === "failed") return { text: "model failed", cls: "bad" };
  if (v.record.draining) return { text: "being emptied", cls: "warn" };
  if (!v.record.lend) return { text: "not lending", cls: "warn" };
  if (v.isMe) return { text: "this PC", cls: "good" };
  return { text: "online", cls: "good" };
}

/** What a device is doing, in one line. */
export function activity(r: NodeRecord): string {
  const i = r.instance;
  if (i) {
    const where = i.kind === "pipeline" ? `split over ${i.stages.length} PCs` : "whole model";
    if (i.state === "loading") return `Loading ${i.model} (${where})…`;
    if (i.state === "failed") return `Could not run ${i.model}`;
    if (i.state === "stopping") return `Stopping ${i.model}…`;
    return `Running ${i.model} · ${where} · ${i.busy}/${i.slots} slots busy${i.tokS > 0 ? ` · ${i.tokS.toFixed(0)} tok/s` : ""}`;
  }
  if (r.share)
    return `Holding layers ${r.share.layerFrom + 1}–${r.share.layerFrom + r.share.layers} of ${r.share.model}${r.share.running ? "" : " (starting)"}`;
  if (r.transfers.length) return `Copying ${r.transfers.map((t) => t.file).join(", ")}`;
  return r.lend ? "Idle — ready for work" : "Idle";
}

/** The accelerators of a device as text, such as "RTX 5060 Ti · 16 GB". */
export function gpuLine(h: Hw): string {
  if (!h.gpus.length) return "";
  return h.gpus
    .map(
      (g) =>
        `${g.name.replace(/^NVIDIA GeForce /, "").replace(/^AMD Radeon /, "")} · ${(g.vramTotalMb / 1024).toFixed(0)} GB`,
    )
    .join(", ");
}

export function totalVramMb(h: Hw): number {
  return h.gpus.reduce((s, g) => s + g.vramTotalMb, 0);
}

/** Share of a device's memory in use, 0–100. */
export function usedPct(total: number, free: number): number {
  if (total <= 0) return 0;
  return Math.max(0, Math.min(100, Math.round(((total - free) / total) * 100)));
}

export type ClusterTotals = {
  online: number;
  devices: number;
  ramGb: number;
  vramGb: number;
  gpus: number;
  copies: number;
  tokS: number;
  busy: number;
  slots: number;
};

/** Headline numbers for the whole cluster. */
export function totals(nodes: NodeView[]): ClusterTotals {
  const t: ClusterTotals = {
    online: 0,
    devices: nodes.length,
    ramGb: 0,
    vramGb: 0,
    gpus: 0,
    copies: 0,
    tokS: 0,
    busy: 0,
    slots: 0,
  };
  for (const n of nodes) {
    if (!n.alive) continue;
    t.online++;
    t.ramGb += n.record.hw.ramTotalMb / 1024;
    t.vramGb += totalVramMb(n.record.hw) / 1024;
    t.gpus += n.record.hw.gpus.length;
    const i = n.record.instance;
    if (i && i.state === "ready") {
      t.copies++;
      t.tokS += i.tokS;
      t.busy += i.busy;
      t.slots += i.slots;
    }
  }
  return t;
}

export type ModelRow = {
  file: string;
  size: number;
  nLayer: number;
  cells: { nodeId: string; has: boolean; copying: number | null; hashOk: boolean }[];
  anyone: boolean;
};

/** Which model is on which device (a row per model, a cell per online device). */
export function modelMatrix(nodes: NodeView[]): ModelRow[] {
  const online = nodes.filter((n) => n.alive);
  const files = new Map<string, { size: number; nLayer: number }>();
  for (const n of online) {
    for (const m of n.record.models) {
      const prev = files.get(m.file);
      files.set(m.file, { size: Math.max(prev?.size ?? 0, m.size), nLayer: Math.max(prev?.nLayer ?? 0, m.nLayer) });
    }
    for (const t of n.record.transfers) if (!files.has(t.file)) files.set(t.file, { size: t.total, nLayer: 0 });
  }
  return [...files.entries()]
    .sort((a, b) => a[0].toLowerCase().localeCompare(b[0].toLowerCase()))
    .map(([file, info]) => ({
      file,
      size: info.size,
      nLayer: info.nLayer,
      anyone: online.some((n) => n.record.models.some((m) => m.file === file)),
      cells: online.map((n) => {
        const m = n.record.models.find((x) => x.file === file);
        const t = n.record.transfers.find((x) => x.file === file);
        return {
          nodeId: n.record.nodeId,
          has: !!m,
          copying: !m && t && t.total > 0 ? Math.round((t.done / t.total) * 100) : null,
          hashOk: !!m && m.sha256.length === 64,
        };
      }),
    }));
}

/** Instances running anywhere, with the device that runs each. */
export function allInstances(nodes: NodeView[]): { node: NodeRecord; inst: Instance }[] {
  return nodes.filter((n) => n.alive && n.record.instance).map((n) => ({ node: n.record, inst: n.record.instance! }));
}

/** One cell per layer, coloured by the device index that holds it. */
export function layerCells(stages: Stage[], total: number): { layer: number; device: number }[] {
  const cells: { layer: number; device: number }[] = [];
  stages.forEach((s, di) => {
    for (let k = 0; k < s.layers; k++) cells.push({ layer: s.layerFrom + k + 1, device: di });
  });
  // Fill any gap so the picture always shows the whole model.
  while (cells.length < total) cells.push({ layer: cells.length + 1, device: -1 });
  return cells;
}

/** The message for the person when a plan cannot be made, or the plan in one sentence. */
export function planSummary(p: PlanResult | null): string {
  if (!p) return "";
  if (p.error && !p.instances.length) return p.error;
  const reps = p.instances.filter((i) => i.kind === "replica").length;
  const pipes = p.instances.length - reps;
  const parts: string[] = [];
  if (reps) parts.push(`${reps} full cop${reps === 1 ? "y" : "ies"}`);
  if (pipes) parts.push(`${pipes} split across devices`);
  const speed = p.instances.reduce((s, i) => s + i.estTokS, 0);
  return `${parts.join(" and ")} · about ${speed.toFixed(0)} tokens/s in total`;
}

/** A command a person can paste to try the gateway. */
export function gatewayExample(url: string, key: string): string {
  const auth = key ? ` \\\n  -H "Authorization: Bearer ${key}"` : "";
  return `curl ${url}/chat/completions${auth} \\\n  -H "Content-Type: application/json" \\\n  -d '{"messages":[{"role":"user","content":"Hello!"}]}'`;
}

/** Makes a new API key. `bytes` is injectable so tests are deterministic. */
export function makeKey(bytes: Uint8Array): string {
  return "lanlm-" + [...bytes].map((x) => x.toString(16).padStart(2, "0")).join("");
}

/** Accepts `192.168.1.5`, `192.168.1.5:50500` or `pc-name:50500`; returns host:port or an error. */
export function parseAddress(raw: string, defaultPort = 50500): { addr: string } | { error: string } {
  const t = raw.trim();
  const m = /^([A-Za-z0-9._-]+)(?::(\d{2,5}))?$/.exec(t);
  if (!m) return { error: "Use an address like 192.168.1.20 or 192.168.1.20:50500." };
  const port = m[2] ? Number(m[2]) : defaultPort;
  if (port < 1 || port > 65535) return { error: "The port must be between 1 and 65535." };
  return { addr: `${m[1]}:${port}` };
}

/** Bytes as "4.1 GB" / "820 MB". */
export function bytesText(b: number): string {
  if (b >= 1e9) return `${(b / 1e9).toFixed(1)} GB`;
  return `${Math.round(b / 1e6)} MB`;
}
