// The Cluster screen: devices, models, running models and the shared API.
// Rendering is plain HTML strings like the rest of the app. Everything that comes from another
// device (names, file names, messages) goes through esc() because other devices are not trusted
// to be well-behaved.
import { invoke } from "@tauri-apps/api/core";
import { ic } from "./lib/icons";
import { esc, gb, modelLabel, pct, short } from "./lib/format";
import {
  activity, ago, allInstances, bytesText, deviceState, formatCode, gatewayExample, layerCells, makeKey, modelMatrix,
  parseAddress, planSummary, totals, usedPct,
  type GatewaySettings, type MeshStatus, type NodeView, type PlanResult, type Planned,
} from "./lib/mesh";

type Tab = "devices" | "models" | "run" | "gateway" | "advanced";
type CatalogEntry = { id: string; name: string; file: string; url: string; sizeMb: number; desc: string; vision: boolean };

/** What the screen needs from the rest of the app. Set once at start-up. */
export const hooks = {
  toast: (_text: string) => {},
  render: () => {},
  catalog: (): CatalogEntry[] => [],
  legacyHtml: (): string => "",
  useInChat: (_on: boolean) => {},
  chatUsesCluster: (): boolean => false,
  engine: (): { installed: boolean; variant: string | null } => ({ installed: false, variant: null }),
  installEngine: (_variant: string) => {},
  engineBusy: (): boolean => false,
  isActive: (): boolean => false,
};

export const M = {
  st: null as MeshStatus | null,
  json: "",
  tab: "devices" as Tab,
  err: "",
  busy: new Set<string>(),
  confirm: "",
  createName: "My cluster",
  joinKey: "",
  addr: "",
  showKey: false,
  key: "",
  nameDraft: "",
  run: { model: "", mode: "auto", ctx: 4096, parallel: 2, max: 0 },
  plan: null as PlanResult | null,
  gw: null as GatewaySettings | null,
  addModel: { item: "", node: "", url: "" },
  logs: [] as string[],
  engineVariant: "auto",
};

const COLORS = ["var(--c1)", "var(--c2)", "var(--c3)", "var(--c4)", "var(--c5)"];
const color = (i: number) => (i < 0 ? "var(--faint)" : COLORS[i % COLORS.length]);

// ───────────────────────────── data ─────────────────────────────

/** Reads the latest state of the cluster. Returns true when something changed. */
export async function refreshMesh(): Promise<boolean> {
  try {
    const st = await invoke<MeshStatus>("mesh_status");
    const j = JSON.stringify(st);
    const changed = j !== M.json;
    M.json = j;
    M.st = st;
    if (!M.gw) M.gw = { ...st.gatewaySettings };
    if (!M.run.model) M.run.model = modelMatrix(st.mesh.nodes).find((r) => r.anyone)?.file ?? "";
    if (!M.addModel.node) M.addModel.node = st.mesh.me;
    return changed;
  } catch {
    return false;
  }
}

/** Are we in a cluster that has at least one model ready to answer? */
export function clusterReady(): boolean {
  const st = M.st;
  return !!st?.mesh.joined && st.mesh.nodes.some((n) => n.alive && n.record.instance?.state === "ready");
}

/** A number for the sidebar: devices online (0 when not in a cluster). */
export function deviceCount(): number {
  const st = M.st;
  return st?.mesh.joined ? st.mesh.nodes.filter((n) => n.alive).length : 0;
}

let planTimer: number | undefined;
export function schedulePlan() {
  window.clearTimeout(planTimer);
  planTimer = window.setTimeout(() => void planNow(), 200);
}

async function planNow() {
  const r = M.run;
  if (!M.st?.mesh.joined || !r.model) { M.plan = null; paint(); return; }
  try {
    M.plan = await invoke<PlanResult>("mesh_preview", { model: r.model, mode: r.mode, ctx: r.ctx, parallel: r.parallel, max: r.max });
  } catch (e) {
    M.plan = { instances: [], notes: [], error: String(e), needMb: 0 };
  }
  paint();
}

/** Redraws only the cluster page, so scrolling and typing elsewhere are not disturbed. */
export function paint(force = false) {
  const root = document.querySelector<HTMLElement>("#mesh-root");
  if (!root) return;
  const a = document.activeElement as HTMLElement | null;
  if (!force && a && root.contains(a) && /^(INPUT|SELECT|TEXTAREA)$/.test(a.tagName)) return; // never wipe what someone is typing
  root.innerHTML = inner();
}

async function run<T>(key: string, f: () => Promise<T>, done?: (v: T) => void): Promise<void> {
  M.busy.add(key);
  M.err = "";
  paint(true);
  try {
    const v = await f();
    done?.(v);
  } catch (e) {
    M.err = String(e);
  }
  M.busy.delete(key);
  await refreshMesh();
  paint(true);
}

const busy = (k: string) => M.busy.has(k);
const dis = (k: string) => (busy(k) ? "disabled" : "");

// ───────────────────────────── small pieces ─────────────────────────────

const bar = (p: number, col: string, title = "") =>
  `<div class="mbar" title="${esc(title)}"><i style="width:${Math.max(0, Math.min(100, p))}%;background:${col}"></i></div>`;

function badge(text: string, cls = ""): string {
  return `<span class="badge ${cls}">${esc(text)}</span>`;
}

function layerMap(p: Planned, total: number): string {
  const cells = layerCells(p.stages, total)
    .map((c) => `<i style="background:${color(c.device)}" title="Layer ${c.layer}${c.device >= 0 ? " → " + esc(p.stages[c.device]?.name ?? "") : ""}">${c.layer}</i>`)
    .join("");
  const legend = p.stages
    .map((s, i) => `<span><b style="background:${color(i)}"></b>${esc(short(s.name, 22))} · ${s.layers} layer${s.layers === 1 ? "" : "s"} · ${esc(s.device)}${s.isMaster ? " · runs the chat" : ""}</span>`)
    .join("");
  return `<div class="layermap">${cells}</div><div class="legend">${legend}</div>`;
}

// ───────────────────────────── not in a cluster ─────────────────────────────

function joinPanel(st: MeshStatus): string {
  const m = st.mesh, ps = m.pairStatus;
  let pair = "";
  if (ps.state === "connecting") pair = `<div class="pair"><div class="hint"><span class="spin"></span> Asking the other PC…</div></div>`;
  else if (ps.state === "waiting") pair = `<div class="pair"><div class="hint">Check that the other PC shows <b>the same code</b>, then press <b>Allow</b> there.</div><div class="code">${esc(formatCode(ps.sas))}</div><button class="btn light sm" data-act="mesh-pair-reset">Cancel</button></div>`;
  else if (ps.state === "denied") pair = `<p class="error">The other PC said no.</p>`;
  else if (ps.state === "failed") pair = `<p class="error">${esc(ps.error)}</p>`;
  const nearby = m.nearby.length
    ? m.nearby.map((n) => `<div class="item"><span class="avatar">${ic("cpu")}</span><div class="body"><div class="nm">${esc(n.name)}</div><div class="ds">is open to new PCs · ${esc(n.addrs[0] ?? "")}</div></div>
        <button class="btn sm" data-act="mesh-join-nearby" data-addr="${esc(n.addrs[0] ?? "")}" ${n.addrs.length ? "" : "disabled"}>Ask to join</button></div>`).join("")
    : `<div class="item"><div class="ds">No PCs open to new members were found on this network. On the other PC choose <b>Cluster → Invite a PC</b>.</div></div>`;
  return `<div class="card primary"><div class="ch"><span>${ic("link")} Join a cluster</span></div><div class="cb">
      <p class="hint" style="margin:0 0 10px">On a PC that is already in the cluster, press <b>Invite a PC</b>. It will show up here.</p>
      <div class="list" style="margin-bottom:12px">${nearby}</div>${pair}
      <details style="margin-top:8px"><summary class="hint" style="cursor:pointer">Other ways to join</summary>
        <div class="row-inline" style="margin-top:10px"><input type="text" id="m-addr" placeholder="Address of a PC, like 192.168.1.20" value="${esc(M.addr)}" style="flex:1;min-width:170px" autocomplete="off">
          <button class="btn light" data-act="mesh-join-addr">Ask to join</button></div>
        <div class="row-inline" style="margin-top:10px"><input type="text" id="m-joinkey" placeholder="Paste a cluster key (lanlm1-…)" value="${esc(M.joinKey)}" style="flex:1;min-width:170px" autocomplete="off">
          <button class="btn light" data-act="mesh-join-key" ${dis("join")}>Join</button></div>
      </details></div></div>`;
}

function startScreen(st: MeshStatus): string {
  return `<div><h2>Cluster</h2><p class="lead">Join your PCs into one AI cluster. They find each other on the network, share what they have, and run models together: each PC can serve a whole model, or share the pieces of a model too big for one. Everything between PCs is encrypted, and a PC only joins when you say yes.</p></div>
    <div class="two">
      <div class="card"><div class="ch"><span>${ic("plus")} Start a cluster</span></div><div class="cb">
        <p class="hint" style="margin:0 0 10px">Do this on the first PC. Then invite the others.</p>
        <div class="row-inline"><input type="text" id="m-create-name" value="${esc(M.createName)}" maxlength="40" style="flex:1" autocomplete="off">
          <button class="btn" data-act="mesh-create" ${dis("create")}>Create</button></div></div></div>
      ${joinPanel(st)}
    </div>
    <p class="error">${esc(M.err)}</p>`;
}

// ───────────────────────────── in a cluster ─────────────────────────────

function header(st: MeshStatus): string {
  const m = st.mesh;
  const reqs = m.pairRequests.map((r) => `<div class="item" style="background:var(--warn-soft)"><span class="avatar">${ic("cpu")}</span>
      <div class="body"><div class="nm">${esc(r.name || "A PC")} wants to join</div><div class="ds">Only allow it if it shows this code: <span class="codeinline">${esc(formatCode(r.sas))}</span></div></div>
      <button class="btn sm" data-act="mesh-allow" data-id="${r.id}">Allow</button><button class="btn light sm" data-act="mesh-deny" data-id="${r.id}">Don't allow</button></div>`).join("");
  const invite = m.pairingOpen
    ? `<div class="hint">Open for two minutes. On the new PC choose <b>Cluster</b> and press <b>Ask to join</b>.</div><button class="btn light sm" data-act="mesh-invite-stop" style="margin-top:6px">Close</button>`
    : `<button class="btn" data-act="mesh-invite">${ic("plus")}Invite a PC</button>`;
  const key = M.showKey
    ? `<code style="word-break:break-all">${esc(M.key)}</code><div class="row-inline" style="margin-top:6px"><button class="btn light sm" data-act="mesh-key-copy">${ic("copy")}Copy</button><button class="btn light sm" data-act="mesh-key-toggle">Hide</button></div>`
    : `<button class="btn light sm" data-act="mesh-key-toggle">Show cluster key</button>`;
  const leave = M.confirm === "leave"
    ? `<button class="btn danger sm" data-act="mesh-leave">Yes, leave</button>`
    : `<button class="btn light sm" data-act="mesh-leave-ask">Leave cluster</button>`;
  return `<div class="card"><div class="ch"><span>${ic("network")} ${esc(m.clusterName || "Cluster")} <span class="badge good">${totals(m.nodes).online} online</span></span>${leave}</div>
    <div class="cb"><div class="two" style="gap:18px">
      <div><div class="hint" style="margin-bottom:6px">This PC's name</div><div class="row-inline"><input type="text" id="m-name" value="${esc(M.nameDraft || m.name)}" maxlength="40" style="flex:1" autocomplete="off"><button class="btn light sm" data-act="mesh-rename">Save</button></div>
        <div class="hint" style="margin:12px 0 6px">Cluster key — gives full access, share it only with PCs you trust</div>${key}</div>
      <div><div class="hint" style="margin-bottom:6px">Add a PC</div>${invite}
        <div class="row-inline" style="margin-top:12px"><input type="text" id="m-addr" placeholder="Or connect by address, like 192.168.1.20" value="${esc(M.addr)}" style="flex:1;min-width:150px" autocomplete="off"><button class="btn light sm" data-act="mesh-add-peer">Connect</button></div></div>
    </div>${reqs ? `<div class="list" style="margin-top:14px">${reqs}</div>` : ""}</div></div>`;
}

function summary(st: MeshStatus): string {
  const t = totals(st.mesh.nodes), g = st.gateway;
  const stat = (label: string, value: string, sub = "") => `<div class="stat"><small>${label}</small><b>${value}</b>${sub ? `<div class="hint" style="margin:0">${sub}</div>` : ""}</div>`;
  return `<div class="summary">
    ${stat("PCs online", `${t.online}${t.devices > t.online ? ` of ${t.devices}` : ""}`)}
    ${stat("Memory", `${t.ramGb.toFixed(0)} GB`, t.vramGb > 0 ? `+ ${t.vramGb.toFixed(0)} GB on ${t.gpus} GPU${t.gpus === 1 ? "" : "s"}` : "no GPUs")}
    ${stat("Models running", String(t.copies), t.slots ? `${t.busy} of ${t.slots} slots busy` : "")}
    ${stat("Speed", t.tokS > 0 ? `${t.tokS.toFixed(0)} tok/s` : "—", "all copies together")}
    ${stat("Waiting", g ? String(g.queued) : "—", g ? `${g.inflight} in progress` : "gateway off")}
  </div>`;
}

function deviceCard(v: NodeView, st: MeshStatus): string {
  const r = v.record, hw = r.hw, ds = deviceState(v);
  const node = v.isMe ? "" : r.nodeId;
  const ramPct = usedPct(hw.ramTotalMb, hw.ramFreeMb);
  const gpus = hw.gpus.map((g) => {
    const p = usedPct(g.vramTotalMb, g.vramFreeMb);
    return `<div class="mrow"><span class="nm">${esc(short(g.name.replace(/^NVIDIA GeForce /, ""), 22))}</span>${bar(p, "var(--c4)", `${gb(g.vramTotalMb - g.vramFreeMb)} of ${gb(g.vramTotalMb)} used`)}<span class="gcap">${gb(g.vramTotalMb)} · ${esc(g.backend)}</span></div>`;
  }).join("");
  const speed = r.bench
    ? `<span class="badge good" title="Measured with ${esc(r.bench.model)}">${r.bench.tokS.toFixed(0)} tok/s measured</span>`
    : `<span class="badge" title="Press Measure speed for an exact number">speed estimated</span>`;
  const engine = r.engine.installed ? `<span class="badge">${esc(r.engine.variant || "cpu")} engine</span>` : `<span class="badge bad">engine not installed</span>`;
  const models = r.models.length
    ? r.models.map((m) => `<span class="chip" title="${esc(m.file)}${m.sha256 ? "\nSHA-256 " + esc(m.sha256) : ""}">${ic("box")}${esc(short(modelLabel(m.file), 24))} <small>${bytesText(m.size)}${m.nLayer ? ` · ${m.nLayer} layers` : ""}</small></span>`).join("")
    : `<span class="hint">No models on this PC</span>`;
  const copying = r.transfers.map((t) => {
    const p = t.total ? Math.round((t.done / t.total) * 100) : 0;
    return `<div class="mrow"><span class="nm">${esc(short(t.file, 24))}</span>${bar(p, "var(--accent)")}<span class="gcap">${p}% from ${esc(t.from.join(", ") || "…")}</span></div>`;
  }).join("");
  const links = !v.isMe && v.rttMs !== null ? ` · ${v.rttMs.toFixed(0)} ms away` : "";
  const t = r.tensorCacheMb ? `<span class="badge" title="Weights this PC was sent, kept so the next load is faster">${gb(r.tensorCacheMb)} cached</span>` : "";
  const failed = r.instance?.state === "failed" ? `<p class="error" style="white-space:pre-wrap">${esc(r.instance.error.split("\n").slice(0, 4).join("\n"))}</p>` : "";
  return `<div class="card dev ${v.alive ? "" : "off"}"><div class="ch"><span><span class="dot ${ds.cls}"></span>${esc(r.name)} ${v.isMe ? badge("this PC") : ""}</span>${badge(ds.text + (v.alive ? links : ` · seen ${ago(v.ageMs)}`), ds.cls)}</div>
    <div class="cb">
      <div class="ds hint" style="margin:0 0 10px">${esc(short(hw.cpu.replace(/\(R\)|\(TM\)| with .*$/g, ""), 40))} · ${hw.cores} cores · ${esc(r.os)}</div>
      <div class="mrow"><span class="nm">Memory</span>${bar(ramPct, "var(--c1)", `${gb(hw.ramTotalMb - hw.ramFreeMb)} of ${gb(hw.ramTotalMb)} in use`)}<span class="gcap">${gb(hw.ramTotalMb)}</span></div>${gpus}
      <div class="chips" style="margin:10px 0">${engine}${speed}${t}${r.draining ? badge("being emptied", "warn") : ""}${r.lend ? "" : badge("not lending", "warn")}</div>
      <div class="chips" style="margin-bottom:10px">${models}</div>${copying}
      <div class="activity">${esc(activity(r))}</div>${failed}
      ${v.alive ? `<div class="row-inline" style="margin-top:12px;flex-wrap:wrap">
        <label class="row-inline" title="Let the cluster put models and pieces of models on this PC"><span class="switch"><input type="checkbox" data-mesh-lend="${esc(node || st.mesh.me)}" ${r.lend ? "checked" : ""}><i></i></span>Lend</label>
        <label class="row-inline" title="Stop giving this PC new work"><span class="switch"><input type="checkbox" data-mesh-drain="${esc(node || st.mesh.me)}" ${r.draining ? "checked" : ""}><i></i></span>Empty</label>
        <span style="flex:1"></span><button class="btn light sm" data-act="mesh-bench" data-node="${esc(node)}" ${dis("bench" + node)} title="Run a short speed test with the smallest model on that PC">${busy("bench" + node) ? "Measuring…" : "Measure speed"}</button></div>` : ""}
    </div></div>`;
}

function devicesTab(st: MeshStatus): string {
  return `<div class="two">${st.mesh.nodes.map((v) => deviceCard(v, st)).join("")}</div>`;
}

function modelsTab(st: MeshStatus): string {
  const nodes = st.mesh.nodes.filter((n) => n.alive);
  const rows = modelMatrix(st.mesh.nodes);
  const head = nodes.map((n) => `<th title="${esc(n.record.name)}">${esc(short(n.record.name, 12))}</th>`).join("");
  const body = rows.map((r) => {
    const cells = r.cells.map((c) => {
      const nid = c.nodeId === st.mesh.me ? "" : c.nodeId;
      if (c.has) return `<td><span class="badge good" title="${c.hashOk ? "Verified by hash" : "Hash not computed yet"}">${ic("check")}${c.hashOk ? "" : "…"}</span> <button class="ibtn" data-act="mesh-model-del" data-node="${esc(nid)}" data-file="${esc(r.file)}" title="Remove from this PC">${ic("trash")}</button></td>`;
      if (c.copying !== null) return `<td><span class="badge">${c.copying}%</span> <button class="ibtn" data-act="mesh-model-cancel" data-node="${esc(nid)}" data-file="${esc(r.file)}" title="Stop copying">${ic("x")}</button></td>`;
      return r.anyone ? `<td><button class="btn light sm" data-act="mesh-model-copy" data-node="${esc(nid)}" data-file="${esc(r.file)}" ${dis("copy" + nid + r.file)}>Copy here</button></td>` : `<td></td>`;
    }).join("");
    return `<tr><td class="mname"><b>${esc(modelLabel(r.file))}</b><div class="hint" style="margin:0">${bytesText(r.size)}${r.nLayer ? ` · ${r.nLayer} layers` : ""}</div></td>${cells}</tr>`;
  }).join("");
  const cat = hooks.catalog();
  const sel = M.addModel.item;
  const opts = cat.map((c) => `<option value="${esc(c.id)}" ${c.id === sel ? "selected" : ""}>${esc(c.name)} (${gb(c.sizeMb)})</option>`).join("");
  const nodeOpts = nodes.map((n) => `<option value="${esc(n.record.nodeId)}" ${n.record.nodeId === M.addModel.node ? "selected" : ""}>${esc(n.record.name)}</option>`).join("");
  return `<div class="card"><div class="ch">Which PC has which model</div><div class="cb" style="padding-top:8px;overflow:auto">
      ${rows.length ? `<table class="mtable"><thead><tr><th></th>${head}</tr></thead><tbody>${body}</tbody></table>` : `<p class="hint">No PC has a model yet. Add one below.</p>`}
      <p class="hint" style="margin:10px 0 0">Copies go PC to PC over your network when a neighbour has the model, and each piece is checked before it is kept. The internet is used only for what nobody here has.</p></div></div>
    <div class="card"><div class="ch">${ic("download")} Add a model to the cluster</div><div class="cb">
      <div class="row-inline" style="flex-wrap:wrap"><select id="m-add-item" style="flex:1;min-width:200px"><option value="">Choose a model…</option>${opts}</select>
        <span class="hint">onto</span><select id="m-add-node" style="min-width:140px">${nodeOpts}</select></div>
      <div class="row-inline" style="margin-top:10px"><input type="text" id="m-add-url" placeholder="…or paste an https:// link to a .gguf file" value="${esc(M.addModel.url)}" style="flex:1;min-width:200px" autocomplete="off"></div>
      <div class="row-inline" style="margin-top:10px;justify-content:flex-end"><button class="btn" data-act="mesh-add-model" ${dis("addmodel")}>Download</button></div></div></div>`;
}

function planBlock(st: MeshStatus): string {
  const p = M.plan;
  if (!p) return `<p class="hint">Choose a model to see where it would run.</p>`;
  const rows = modelMatrix(st.mesh.nodes);
  const layers = rows.find((r) => r.file === M.run.model)?.nLayer ?? 0;
  const items = p.instances.map((i) => `<div class="planrow"><div class="row-inline" style="justify-content:space-between"><span>${badge(i.kind === "replica" ? "whole model" : "split over " + i.stages.length + " PCs", i.kind === "replica" ? "good" : "warn")} <span class="hint">${esc(i.note)}</span></span><b>≈ ${i.estTokS.toFixed(0)} tok/s</b></div>${layerMap(i, layers)}${i.needsFetch ? `<div class="hint">The model will be copied to ${esc(i.stages[0]?.name ?? "that PC")} first.</div>` : ""}</div>`).join("");
  const notes = p.notes.map((n) => `<p class="hint">${esc(n)}</p>`).join("");
  return `${p.instances.length ? `<p class="hint" style="margin:0 0 10px"><b>${esc(planSummary(p))}</b> · needs about ${gb(p.needMb)} per copy</p>${items}` : ""}${p.error ? `<p class="${p.instances.length ? "hint" : "error"}">${esc(p.error)}</p>` : ""}${notes}`;
}

function runTab(st: MeshStatus): string {
  const rows = modelMatrix(st.mesh.nodes).filter((r) => r.anyone);
  const opts = rows.map((r) => `<option value="${esc(r.file)}" ${r.file === M.run.model ? "selected" : ""}>${esc(modelLabel(r.file))} (${bytesText(r.size)})</option>`).join("");
  const modes: [string, string][] = [["auto", "Automatic — full copies where they fit, split only when needed"], ["replicas", "Full copies only"], ["pipeline", "Split every copy across PCs"]];
  const dep = st.deployments.map((d) => {
    const ds = st.deployStatus[d.id];
    const insts = allInstances(st.mesh.nodes).filter((x) => x.inst.deployment === d.id);
    const list = insts.map(({ node, inst }) => `<div class="mrow"><span class="nm">${esc(short(node.name, 18))}</span><span class="badge ${inst.state === "ready" ? "good" : inst.state === "failed" ? "bad" : "warn"}">${esc(inst.state)}</span><span class="gcap" style="flex:1;text-align:left">${inst.kind === "pipeline" ? `split over ${inst.stages.length} PCs · ` : ""}${inst.busy}/${inst.slots} busy${inst.tokS > 0 ? ` · ${inst.tokS.toFixed(0)} tok/s` : ""}</span></div>`).join("");
    return `<div class="item" style="align-items:flex-start"><div class="body"><div class="nm">${esc(modelLabel(d.model))} <span class="badge">${esc(d.mode)}</span></div>
        <div class="ds ${ds?.isError ? "error" : ""}" style="${ds?.isError ? "white-space:pre-wrap" : ""}">${esc(ds?.message ?? "")}</div><div style="margin-top:8px">${list}</div></div>
      <div style="display:flex;flex-direction:column;gap:6px"><button class="btn sm" data-act="mesh-chat" ${clusterReady() ? "" : "disabled"}>${hooks.chatUsesCluster() ? "Chatting with it" : "Chat with it"}</button>
        <button class="btn danger sm" data-act="mesh-stop" data-id="${esc(d.id)}" ${dis("stop" + d.id)}>Stop</button></div></div>`;
  }).join("");
  return `<div class="card primary"><div class="ch"><span>${ic("bolt")} Run a model on the cluster</span></div><div class="cb">
      ${rows.length ? `<div class="two" style="gap:14px">
        <label class="fld">Model<select id="m-run-model">${opts}</select></label>
        <label class="fld">How<select id="m-run-mode">${modes.map(([v, t]) => `<option value="${v}" ${M.run.mode === v ? "selected" : ""}>${esc(t)}</option>`).join("")}</select></label>
        <label class="fld">Context per copy (tokens)<input type="number" id="m-run-ctx" value="${M.run.ctx}" min="512" step="512"></label>
        <label class="fld">Requests at once, per copy<input type="number" id="m-run-par" value="${M.run.parallel}" min="1" max="64"></label>
        <label class="fld">Most copies (0 = as many as fit)<input type="number" id="m-run-max" value="${M.run.max}" min="0" max="64"></label></div>
        <div style="margin-top:16px">${planBlock(st)}</div>
        <div class="row-inline" style="margin-top:14px;justify-content:flex-end"><button class="btn" data-act="mesh-deploy" ${M.plan?.instances.length ? "" : "disabled"} ${dis("deploy")}>Start</button></div>`
        : `<p class="hint">Add a model on the <b>Models</b> tab first.</p>`}
      <p class="error">${esc(M.err)}</p></div></div>
    <div class="card"><div class="ch">Running</div><div class="cb" style="padding:0"><div class="list" style="border:0">${dep || `<div class="item"><div class="ds">Nothing is running yet. When you start a model the cluster keeps it running: if a PC goes away the model is placed again on the others, and PCs that join are put to use.</div></div>`}</div></div></div>`;
}

function gatewayTab(st: MeshStatus): string {
  const g = M.gw ?? st.gatewaySettings, live = st.gateway;
  const keys = g.apiKeys.map((k, i) => `<div class="mrow"><code style="flex:1;overflow:hidden;text-overflow:ellipsis">${esc(k)}</code><button class="ibtn" data-act="mesh-gw-key-copy" data-i="${i}" title="Copy">${ic("copy")}</button><button class="ibtn" data-act="mesh-gw-key-rm" data-i="${i}" title="Remove">${ic("trash")}</button></div>`).join("");
  const backends = live ? live.backends.map((b) => `<div class="mrow"><span class="nm">${esc(short(b.nodeName, 18))}</span>${bar(pct(b.busy, b.slots), "var(--c3)")}<span class="gcap">${b.busy}/${b.slots} busy</span></div>`).join("") : "";
  const stats = live ? `<div class="summary" style="margin-top:12px"><div class="stat"><small>Servers</small><b>${live.backends.length}</b></div><div class="stat"><small>In progress</small><b>${live.inflight}</b></div><div class="stat"><small>Waiting</small><b>${live.queued}</b></div><div class="stat"><small>Served</small><b>${live.total}</b></div><div class="stat"><small>Retried</small><b>${live.retried}</b></div><div class="stat"><small>Failed</small><b>${live.failed}</b></div></div>${backends ? `<div style="margin-top:12px">${backends}</div>` : ""}` : "";
  const key = g.apiKeys[0] ?? "";
  return `<div class="card primary"><div class="ch"><span>${ic("server")} One address for the whole cluster</span>${live ? badge("running", "good") : badge("off")}</div><div class="cb">
      <p class="hint" style="margin:0 0 12px">Apps and tools use the OpenAI API at this address. Each request goes to the PC with the most room, waits in line when everything is busy, stays on the same PC for a whole conversation (so its memory of the chat is reused), and moves to another PC if one fails.</p>
      <div class="row-inline" style="flex-wrap:wrap;gap:16px">
        <label class="row-inline">Port <input type="number" id="m-gw-port" class="mini" style="width:84px" value="${g.port}" min="1024" max="65535"></label>
        <label class="row-inline"><span class="switch"><input type="checkbox" id="m-gw-net" ${g.network ? "checked" : ""}><i></i></span>Let other computers use it</label>
        <span style="flex:1"></span>${live ? `<button class="btn danger" data-act="mesh-gw-stop">Turn off</button>` : ""}<button class="btn" data-act="mesh-gw-save" ${dis("gw")}>${live ? "Apply" : "Turn on"}</button></div>
      <div class="hint" style="margin:14px 0 6px">API keys ${g.network ? "(required to share on the network)" : "(optional on this PC)"}</div>${keys || `<p class="hint" style="margin:0">No keys.</p>`}
      <div style="margin-top:8px"><button class="btn light sm" data-act="mesh-gw-key-add">${ic("plus")}New key</button></div>
      ${live ? `<div class="pair" style="margin-top:14px"><div class="hint">Address</div><div class="row-inline" style="justify-content:space-between"><code>${esc(live.url)}</code><button class="btn light sm" data-act="mesh-gw-copy-url">${ic("copy")}Copy</button></div>
        <pre class="codeblock-lite">${esc(gatewayExample(live.url, key))}</pre><button class="btn light sm" data-act="mesh-gw-copy-example">${ic("copy")}Copy example</button></div>${stats}` : ""}
      <p class="error">${esc(M.err)}</p></div></div>`;
}

function advancedTab(st: MeshStatus): string {
  const eng = hooks.engine();
  const variants = ["auto", "cpu", "vulkan", "cuda-13.4", "cuda-12.4"];
  const logs = M.logs.length ? esc(M.logs.slice(-200).join("\n")) : "No messages yet.";
  return `<div class="card"><div class="ch"><span>${ic("cpu")} Engine and GPU</span>${eng.installed ? badge((eng.variant ?? "cpu") + " build installed", "good") : badge("not installed", "bad")}</div><div class="cb">
      <p class="hint" style="margin:0 0 10px">LanLM picks the llama.cpp build that fits this PC's graphics card (NVIDIA CUDA, or Vulkan for AMD and Intel) and checks that it really sees the GPU. A PC without a supported GPU uses the CPU build.</p>
      <div class="row-inline"><select id="m-engine-variant">${variants.map((v) => `<option value="${v}" ${M.engineVariant === v ? "selected" : ""}>${v === "auto" ? "Choose automatically" : v}</option>`).join("")}</select>
        <button class="btn" data-act="mesh-engine" ${hooks.engineBusy() ? "disabled" : ""}>${eng.installed ? "Reinstall engine" : "Install engine"}</button></div></div></div>
    <div class="card"><div class="ch">This PC on the network</div><div class="cb"><p class="hint" style="margin:0 0 6px">Other PCs can connect to this PC at (port ${st.mesh.port}):</p>${st.mesh.addrs.map((a) => `<code>${esc(a)}</code>`).join(" ") || `<span class="hint">no network address found</span>`}
      <p class="hint" style="margin:10px 0 0">Allow TCP port ${st.mesh.port} in the firewall for PCs on your private network. Cluster traffic is encrypted and needs the cluster key.</p></div></div>
    <div class="card"><div class="ch">Activity <button class="btn light sm" data-act="mesh-logs">${ic("refresh")}Refresh</button></div><div class="cb"><pre class="logbox-lite">${logs}</pre></div></div>
    <details class="card"><summary class="ch" style="cursor:pointer">Add a PC by hand (older method)</summary><div class="cb">${hooks.legacyHtml()}</div></details>`;
}

function inner(): string {
  const st = M.st;
  if (!st) return `<div><h2>Cluster</h2><p class="hint"><span class="spin"></span> Starting…</p></div>`;
  if (!st.mesh.joined) return startScreen(st);
  const tabs: [Tab, string][] = [["devices", "PCs"], ["models", "Models"], ["run", "Run"], ["gateway", "Shared API"], ["advanced", "More"]];
  const body = M.tab === "devices" ? devicesTab(st) : M.tab === "models" ? modelsTab(st) : M.tab === "run" ? runTab(st) : M.tab === "gateway" ? gatewayTab(st) : advancedTab(st);
  return `<div><h2>Cluster</h2></div>${header(st)}${summary(st)}
    <div class="mtabs">${tabs.map(([t, l]) => `<button class="${M.tab === t ? "on" : ""}" data-act="mesh-tab" data-tab="${t}">${l}</button>`).join("")}</div>
    ${M.tab !== "run" && M.tab !== "gateway" && M.err ? `<p class="error">${esc(M.err)}</p>` : ""}${body}`;
}

/** The page: a scrolling shell whose inside is redrawn on its own as the cluster changes. */
export function meshView(): string {
  return `<div class="scroll"><div class="col wide page mesh" id="mesh-root">${inner()}</div></div>`;
}

// ───────────────────────────── actions ─────────────────────────────

const v = (id: string) => (document.querySelector<HTMLInputElement>("#" + id)?.value ?? "").trim();

/** Handles a click on a cluster control. Returns true if it was one of ours. */
export function meshAct(act: string, d: DOMStringMap): boolean {
  switch (act) {
    case "mesh-tab":
      M.tab = d.tab as Tab; M.err = "";
      if (M.tab === "run") schedulePlan();
      if (M.tab === "advanced") void invoke<string[]>("mesh_logs").then((l) => { M.logs = l; paint(true); });
      paint(true); return true;
    case "mesh-create": {
      const name = v("m-create-name") || "My cluster"; M.createName = name;
      void run("create", () => invoke<string>("mesh_create", { name }), () => { M.showKey = false; }); return true;
    }
    case "mesh-join-key": { M.joinKey = v("m-joinkey"); void run("join", () => invoke("mesh_join", { key: M.joinKey })); return true; }
    case "mesh-join-nearby": if (d.addr) void run("pair", () => invoke("mesh_pair_start", { addr: d.addr })); return true;
    case "mesh-join-addr": {
      const a = parseAddress(v("m-addr")); M.addr = v("m-addr");
      if ("error" in a) { M.err = a.error; paint(true); return true; }
      void run("pair", () => invoke("mesh_pair_start", { addr: a.addr })); return true;
    }
    case "mesh-pair-reset": void run("pairreset", () => invoke("mesh_pair_reset")); return true;
    case "mesh-invite": void run("invite", () => invoke("mesh_pair_open")); return true;
    case "mesh-invite-stop": void run("invite", () => invoke("mesh_pair_close")); return true;
    case "mesh-allow": void run("decide", () => invoke("mesh_pair_decide", { id: Number(d.id), allow: true })); return true;
    case "mesh-deny": void run("decide", () => invoke("mesh_pair_decide", { id: Number(d.id), allow: false })); return true;
    case "mesh-rename": { const n = v("m-name"); if (n) { M.nameDraft = ""; void run("rename", () => invoke("mesh_set_name", { name: n })); } return true; }
    case "mesh-add-peer": {
      const a = parseAddress(v("m-addr")); M.addr = v("m-addr");
      if ("error" in a) { M.err = a.error; paint(true); return true; }
      void run("peer", () => invoke("mesh_add_peer", { addr: a.addr }), () => { M.addr = ""; hooks.toast("Connecting…"); }); return true;
    }
    case "mesh-key-toggle":
      if (M.showKey) { M.showKey = false; M.key = ""; paint(true); } else void invoke<string | null>("mesh_key").then((k) => { M.key = k ?? ""; M.showKey = true; paint(true); });
      return true;
    case "mesh-key-copy": void navigator.clipboard?.writeText(M.key).then(() => hooks.toast("Cluster key copied")); return true;
    case "mesh-leave-ask": M.confirm = "leave"; paint(true); window.setTimeout(() => { M.confirm = ""; paint(true); }, 4000); return true;
    case "mesh-leave": M.confirm = ""; void run("leave", () => invoke("mesh_leave"), () => { M.showKey = false; M.plan = null; }); return true;
    case "mesh-bench": void run("bench" + (d.node ?? ""), () => invoke("mesh_bench", { node: d.node ?? "" }), (r) => hooks.toast(`${(r as { tokS: number }).tokS.toFixed(1)} tokens/s`)); return true;
    case "mesh-model-copy": void run("copy" + (d.node ?? "") + d.file, () => invoke("mesh_fetch", { node: d.node ?? "", file: d.file, url: null }), () => hooks.toast("Copied")); return true;
    case "mesh-model-cancel": void run("cancel", () => invoke("mesh_cancel_fetch", { node: d.node ?? "", file: d.file })); return true;
    case "mesh-model-del": void run("del", () => invoke("mesh_delete_model", { node: d.node ?? "", file: d.file })); return true;
    case "mesh-add-model": {
      const cat = hooks.catalog().find((c) => c.id === v("m-add-item"));
      const url = v("m-add-url"); M.addModel.url = url; M.addModel.node = v("m-add-node");
      const target = M.addModel.node === M.st?.mesh.me ? "" : M.addModel.node;
      let file = cat?.file ?? "", link = cat?.url ?? "";
      if (!cat && url) { link = url; file = decodeURIComponent(url.split("?")[0].split("/").pop() ?? ""); }
      if (!link || !file.toLowerCase().endsWith(".gguf") || !link.startsWith("https://")) { M.err = "Choose a model, or paste an https:// link that ends in .gguf"; paint(true); return true; }
      void run("addmodel", () => invoke("mesh_fetch", { node: target, file, url: link }), () => { M.addModel.url = ""; hooks.toast("Model added"); }); return true;
    }
    case "mesh-deploy": {
      const r = M.run;
      void run("deploy", () => invoke("mesh_deploy", { deployment: { id: "", model: r.model, mode: r.mode, ctx: r.ctx, parallel: r.parallel, nodes: [], maxInstances: r.max, url: "", sha256: "" } }), () => hooks.toast("Starting…")); return true;
    }
    case "mesh-stop": void run("stop" + d.id, () => invoke("mesh_undeploy", { id: d.id })); return true;
    case "mesh-chat": hooks.useInChat(!hooks.chatUsesCluster()); paint(true); return true;
    case "mesh-gw-save": {
      const g = M.gw ?? { enabled: true, port: 8080, network: false, apiKeys: [] };
      g.port = Math.round(Number(v("m-gw-port"))) || 8080; g.network = !!document.querySelector<HTMLInputElement>("#m-gw-net")?.checked; g.enabled = true;
      M.gw = g;
      void run("gw", () => invoke("mesh_gateway", { settings: { ...g, apiKeys: g.apiKeys } })); return true;
    }
    case "mesh-gw-stop": void run("gw", () => invoke("mesh_gateway", { settings: { ...(M.gw ?? { port: 8080, network: false, apiKeys: [] }), enabled: false } })); return true;
    case "mesh-gw-key-add": {
      const b = new Uint8Array(18); crypto.getRandomValues(b);
      (M.gw ??= { enabled: false, port: 8080, network: false, apiKeys: [] }).apiKeys.push(makeKey(b)); paint(true); return true;
    }
    case "mesh-gw-key-rm": M.gw?.apiKeys.splice(Number(d.i), 1); paint(true); return true;
    case "mesh-gw-key-copy": { const k = M.gw?.apiKeys[Number(d.i)]; if (k) void navigator.clipboard?.writeText(k).then(() => hooks.toast("API key copied")); return true; }
    case "mesh-gw-copy-url": if (M.st?.gateway) void navigator.clipboard?.writeText(M.st.gateway.url).then(() => hooks.toast("Address copied")); return true;
    case "mesh-gw-copy-example": if (M.st?.gateway) void navigator.clipboard?.writeText(gatewayExample(M.st.gateway.url, M.gw?.apiKeys[0] ?? "")).then(() => hooks.toast("Example copied")); return true;
    case "mesh-logs": void invoke<string[]>("mesh_logs").then((l) => { M.logs = l; paint(true); }); return true;
    case "mesh-engine": hooks.installEngine(M.engineVariant); return true;
    default: return false;
  }
}

/** Handles typing and choices. Returns true if the control was one of ours. */
export function meshInput(t: HTMLInputElement | HTMLSelectElement, kind: "input" | "change"): boolean {
  if (t.dataset.meshLend !== undefined && kind === "change") { void run("lend", () => invoke("mesh_lend", { node: t.dataset.meshLend === M.st?.mesh.me ? "" : t.dataset.meshLend, on: (t as HTMLInputElement).checked })); return true; }
  if (t.dataset.meshDrain !== undefined && kind === "change") { void run("drain", () => invoke("mesh_drain", { node: t.dataset.meshDrain === M.st?.mesh.me ? "" : t.dataset.meshDrain, on: (t as HTMLInputElement).checked })); return true; }
  switch (t.id) {
    case "m-name": M.nameDraft = t.value; return true;
    case "m-addr": M.addr = t.value; return true;
    case "m-joinkey": M.joinKey = t.value; return true;
    case "m-create-name": M.createName = t.value; return true;
    case "m-add-url": M.addModel.url = t.value; return true;
    case "m-add-node": M.addModel.node = t.value; return true;
    case "m-add-item": M.addModel.item = t.value; return true;
    case "m-engine-variant": M.engineVariant = t.value; return true;
    case "m-run-model": M.run.model = t.value; schedulePlan(); return true;
    case "m-run-mode": M.run.mode = t.value; schedulePlan(); return true;
    case "m-run-ctx": M.run.ctx = Math.max(512, Math.round(Number(t.value)) || 4096); if (kind === "change") schedulePlan(); return true;
    case "m-run-par": M.run.parallel = Math.max(1, Math.round(Number(t.value)) || 1); if (kind === "change") schedulePlan(); return true;
    case "m-run-max": M.run.max = Math.max(0, Math.round(Number(t.value)) || 0); if (kind === "change") schedulePlan(); return true;
    case "m-gw-port": if (M.gw) M.gw.port = Math.round(Number(t.value)) || 8080; return true;
    case "m-gw-net": if (M.gw) M.gw.network = (t as HTMLInputElement).checked; return true;
    default: return false;
  }
}

let timer: number | undefined;
/** Keeps the cluster page fresh while it is on screen, and the sidebar badge otherwise. */
export function startPolling(onBadge: () => void) {
  window.clearInterval(timer);
  timer = window.setInterval(async () => {
    const changed = await refreshMesh();
    if (!changed) return;
    if (hooks.isActive()) paint(); else onBadge();
  }, 1500);
}
