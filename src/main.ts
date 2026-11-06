import { invoke, Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./style.css";
import { ic } from "./lib/icons";
import { esc, gb, md, modelLabel, pct, short } from "./lib/format";
import { lendableMb, parseWorkerInput } from "./lib/cluster";
import * as mesh from "./mesh";

// ═════════════════════════════ types ═════════════════════════════
type Hw = { cpu: string; physicalCores: number; logicalCores: number; totalRamMb: number; availRamMb: number; recThreads: number; recCtx: number };
type Engine = { installed: boolean; version: string | null; path: string | null; variant?: string | null; hasRpc?: boolean };
type CatalogItem = { id: string; name: string; file: string; url: string; sizeMb: number; desc: string; vision: boolean; projUrl: string | null; projFile: string | null; projMb: number };
type Installed = { file: string; sizeMb: number; vision: boolean };
type Server = { running: boolean; port: number | null; model: string | null; note?: string | null; apiUrl?: string | null; vision?: boolean };
type QueuedMsg = { text: string; images: string[] };
type Worker = { endpoint: string; ramMb: number; ok?: boolean; off?: boolean };
type WorkerStatus = { running: boolean; port: number | null; addresses: string[]; lan: boolean; pairing: string | null; ramMb: number };
type Cluster = { enabled: boolean; forceShare: boolean; workers: Worker[]; workerPort: number; workerLan: boolean };
type DevicePlan = { name: string; kind: "master" | "worker"; capMb: number; layers: number; mb: number };
type Preview = { nLayer: number; fileMb: number; needMb: number; usesWorkers: boolean; devices: DevicePlan[]; note: string; error: string | null };
type Progress = { event: "status"; data: { text: string } } | { event: "bytes"; data: { done: number; total: number } } | { event: "done" };
type ChatEvent =
  | { event: "delta"; data: { text: string } }
  | { event: "reasoning"; data: { text: string } }
  | { event: "stats"; data: { promptTps: number; genTps: number; tokens: number; contextTokens: number } }
  | { event: "done" };
type Msg = { role: "user" | "assistant"; content: string; reasoning?: string; stats?: string; images?: string[] };
type Chat = { id: string; title: string; messages: Msg[]; updated: number };
type Settings = {
  ctx: number; threads: number; parallel: number; temperature: number; maxTokens: number; system: string; lastModel: string;
  apiEnabled: boolean; apiPort: number; apiNetwork: boolean; apiKey: string;
  theme: "system" | "light" | "dark"; scale: number; thinking: "collapsed" | "open" | "hidden";
};
type View = "chat" | "models" | "cluster";
type Tab = "general" | "engine" | "api" | "appearance" | "about";
type Menu = { kind: "chat" | "model" | "plus"; left: number; top?: number; bottom?: number };

// ═════════════════════════════ state ═════════════════════════════
const S = {
  view: "chat" as View,
  side: true,
  right: true,
  hw: null as Hw | null,
  engine: { installed: false, version: null, path: null } as Engine,
  catalog: [] as CatalogItem[],
  installed: [] as Installed[],
  server: { running: false, port: null, model: null } as Server,
  loaded: { ctx: 4096, parallel: 1 },
  loading: false,
  loadError: "",
  engineMsg: "",
  engineBusy: false,
  downloads: new Map<string, { name: string; done: number; total: number; error?: string }>(),
  chats: [] as Chat[],
  activeId: "",
  generating: false,
  queue: [] as QueuedMsg[],
  attach: [] as string[],
  ctxTokens: 0,
  lastTps: 0,
  logs: [] as string[],
  searchOpen: false,
  query: "",
  renamingId: "",
  filter: "all" as "all" | "fits" | "installed" | "vision",
  menu: null as Menu | null,
  modal: null as Tab | null,
  cluster: { enabled: false, forceShare: false, workers: [], workerPort: 50052, workerLan: true } as Cluster,
  worker: { running: false, port: null, addresses: [], lan: false, pairing: null, ramMb: 0 } as WorkerStatus,
  clusterMsg: "",
  useCluster: false,
  addMsg: "",
  previewModel: "",
  preview: null as Preview | null,
  previewErr: "",
  settings: {
    ctx: 4096, threads: 4, parallel: 1, temperature: 0.7, maxTokens: 1024, system: "You are a helpful, concise assistant.", lastModel: "",
    apiEnabled: false, apiPort: 8080, apiNetwork: false, apiKey: "", theme: "system", scale: 1, thinking: "collapsed",
  } as Settings,
};

const store = {
  get<T>(k: string, d: T): T { try { const v = localStorage.getItem(k); return v ? (JSON.parse(v) as T) : d; } catch { return d; } },
  set(k: string, v: unknown) { try { localStorage.setItem(k, JSON.stringify(v)); } catch { /* storage unavailable */ } },
};
const saveChats = () => store.set("chats", S.chats);
const saveSettings = () => store.set("settings", S.settings);
const saveCluster = () => store.set("cluster", S.cluster);
const savePrefs = () => store.set("prefs", { side: S.side, right: S.right });

// ═════════════════════════════ helpers ═════════════════════════════
const $ = <T extends HTMLElement>(sel: string) => document.querySelector(sel) as T;
const uid = () => Math.random().toString(36).slice(2, 10);
const active = () => S.chats.find((c) => c.id === S.activeId);
const devColor = (i: number) => `var(--c${(i % 5) + 1})`;

function toast(text: string, ms = 3500) {
  document.querySelector(".toast")?.remove();
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = text;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), ms);
}

function fitBadge(sizeMb: number): string {
  const total = S.hw?.totalRamMb ?? 0;
  if (!total) return "";
  const need = sizeMb * 1.2 + 600;
  if (need <= total * 0.6) return `<span class="badge good">Fits this PC</span>`;
  if (need <= total * 0.8) return `<span class="badge warn">Tight fit</span>`;
  return `<span class="badge bad">Too big for this PC</span>`;
}
const fitsPc = (sizeMb: number) => (S.hw ? sizeMb * 1.2 + 600 <= S.hw.totalRamMb * 0.8 : true);

// ═════════════════════════════ data ═════════════════════════════
const refreshInstalled = async () => { S.installed = await invoke<Installed[]>("list_installed"); };
const refreshServer = async () => { S.server = await invoke<Server>("server_status"); };
const refreshEngine = async () => { S.engine = await invoke<Engine>("engine_status"); };

function applyTheme() {
  const r = document.documentElement;
  if (S.settings.theme === "system") r.removeAttribute("data-theme"); else r.dataset.theme = S.settings.theme;
  r.style.setProperty("--scale", String(S.settings.scale));
}

async function boot() {
  S.chats = store.get<Chat[]>("chats", []).map((c) => ({ ...c, updated: c.updated ?? Date.now() }));
  S.settings = { ...S.settings, ...store.get<Partial<Settings>>("settings", {}) };
  S.cluster = { ...S.cluster, ...store.get<Partial<Cluster>>("cluster", {}) };
  const prefs = store.get<{ side: boolean; right: boolean }>("prefs", { side: true, right: true });
  S.side = prefs.side; S.right = prefs.right;
  applyTheme();
  S.hw = await invoke<Hw>("hardware_info");
  if (!store.get("settingsSeen", false)) {
    S.settings.ctx = S.hw.recCtx;
    S.settings.threads = S.hw.recThreads;
    store.set("settingsSeen", true);
    saveSettings();
  }
  S.catalog = await invoke<CatalogItem[]>("catalog");
  mesh.hooks.toast = toast;
  mesh.hooks.render = render;
  mesh.hooks.catalog = () => S.catalog;
  mesh.hooks.legacyHtml = legacyClusterHtml;
  mesh.hooks.useInChat = (on) => { S.useCluster = on; render(); toast(on ? "Chats now go to the whole cluster" : "Chats use the model on this PC"); };
  mesh.hooks.chatUsesCluster = () => S.useCluster;
  mesh.hooks.engine = () => ({ installed: S.engine.installed, variant: S.engine.variant ?? null });
  mesh.hooks.installEngine = (variant) => { void installEngine(variant); };
  mesh.hooks.engineBusy = () => S.engineBusy;
  mesh.hooks.isActive = () => S.view === "cluster";
  await mesh.refreshMesh();
  mesh.startPolling(() => {
    // Off the cluster page only the sidebar badge depends on the cluster.
    if (S.view !== "cluster") render();
  });
  S.worker = await invoke<WorkerStatus>("worker_status");
  await Promise.all([refreshEngine(), refreshInstalled(), refreshServer()]);
  S.logs = await invoke<string[]>("recent_logs");
  if (S.server.running) { S.loaded = { ctx: S.settings.ctx, parallel: S.settings.parallel }; }
  if (!S.chats.length) newChat(false);
  S.activeId = S.activeId || S.chats[0].id;
  S.previewModel = S.server.model ?? S.settings.lastModel ?? "";
  await listen<string>("engine-log", (e) => {
    S.logs.push(e.payload);
    if (S.logs.length > 500) S.logs.shift();
    const box = document.querySelector<HTMLElement>("#logbox");
    if (box) { box.textContent = S.logs.join("\n"); box.scrollTop = box.scrollHeight; }
  });
  render();
}

// ═════════════════════════════ chat actions ═════════════════════════════
function newChat(doRender = true) {
  const cur = S.chats[0];
  if (cur && cur.id === S.activeId && !cur.messages.length && doRender) { S.view = "chat"; render(); return; }
  const c: Chat = { id: uid(), title: "New chat", messages: [], updated: Date.now() };
  S.chats.unshift(c);
  S.activeId = c.id;
  S.view = "chat";
  S.ctxTokens = 0;
  saveChats();
  if (doRender) render();
}

function deleteChat(id: string) {
  S.chats = S.chats.filter((c) => c.id !== id);
  if (!S.chats.length) newChat(false);
  if (S.activeId === id) S.activeId = S.chats[0].id;
  saveChats();
  render();
}

function chatMarkdown(c: Chat): string {
  return `# ${c.title}\n\n` + c.messages.map((m) => `**${m.role === "user" ? "You" : "Assistant"}:**\n\n${m.content}`).join("\n\n---\n\n");
}

/** Chat goes to the whole cluster when that is chosen and a model is running there. */
const chatViaCluster = (): boolean => S.useCluster && mesh.clusterReady();

async function generate(chat: Chat) {
  const reply: Msg = { role: "assistant", content: "" };
  chat.messages.push(reply);
  S.generating = true;
  render();
  // Only the newest image message is sent as pictures (they are expensive to read);
  // earlier ones are mentioned in words.
  const past = chat.messages.slice(0, -1);
  let lastImg = -1;
  past.forEach((m, i) => { if (m.images?.length) lastImg = i; });
  const history = [
    ...(S.settings.system.trim() ? [{ role: "system", content: S.settings.system }] : []),
    ...past.map((m, i) => {
      if (m.role === "user" && m.images?.length) {
        if (i === lastImg) {
          return { role: m.role, content: [{ type: "text", text: m.content || "Describe this image." }, ...m.images.map((u) => ({ type: "image_url", image_url: { url: u } }))] };
        }
        return { role: m.role, content: `${m.content} [an earlier image was attached]`.trim() };
      }
      return { role: m.role, content: m.content };
    }),
  ];
  const ch = new Channel<ChatEvent>();
  ch.onmessage = (m) => {
    if (m.event === "delta") reply.content += m.data.text;
    else if (m.event === "reasoning") reply.reasoning = (reply.reasoning ?? "") + m.data.text;
    else if (m.event === "stats") {
      reply.stats = `${m.data.genTps.toFixed(1)} tok/s · ${m.data.tokens} tokens`;
      S.lastTps = m.data.genTps;
      S.ctxTokens = m.data.contextTokens;
    }
    if (active()?.id === chat.id) paintLastMessage();
  };
  try {
    await invoke("chat_stream", { req: { messages: history, temperature: S.settings.temperature, maxTokens: S.settings.maxTokens, cluster: chatViaCluster() }, onEvent: ch });
  } catch (e) {
    reply.content += (reply.content ? "\n\n" : "") + "⚠ " + String(e);
  }
  S.generating = false;
  chat.updated = Date.now();
  saveChats();
  render();
  const next = S.queue.shift();
  if (next) void send(next.text, next.images);
}

async function send(text?: string, images?: string[]) {
  const box = document.querySelector<HTMLTextAreaElement>("#input");
  const msg = (text ?? box?.value ?? "").trim();
  const imgs = images ?? [...S.attach];
  const chat = active();
  if ((!msg && !imgs.length) || !chat) return;
  if (!S.server.running && !chatViaCluster()) { toast(S.useCluster ? "No model is running on the cluster yet. Start one on the Cluster page." : "Choose a model below the message box first."); return; }
  if (imgs.length && (chatViaCluster() || !S.server.vision)) { toast("This model can't read images. Load a vision model (marked “Vision”)."); return; }
  if (text === undefined) { if (box) { box.value = ""; box.style.height = "auto"; } S.attach = []; }
  if (S.generating) { S.queue.push({ text: msg, images: imgs }); render(); return; }
  chat.messages.push({ role: "user", content: msg, ...(imgs.length ? { images: imgs } : {}) });
  if (chat.title === "New chat") chat.title = short(msg || "Image chat", 42);
  await generate(chat);
}

// Shrinks a picture so it is quick to read on a CPU and small to keep in a chat.
async function resizeImage(blob: Blob, max = 640): Promise<string> {
  const url = URL.createObjectURL(blob);
  try {
    const img = await new Promise<HTMLImageElement>((res, rej) => {
      const i = new Image();
      i.onload = () => res(i);
      i.onerror = () => rej(new Error("Could not read that image"));
      i.src = url;
    });
    const k = Math.min(1, max / Math.max(img.width, img.height));
    const c = document.createElement("canvas");
    c.width = Math.max(1, Math.round(img.width * k));
    c.height = Math.max(1, Math.round(img.height * k));
    const g = c.getContext("2d")!;
    g.fillStyle = "#fff";
    g.fillRect(0, 0, c.width, c.height);
    g.drawImage(img, 0, 0, c.width, c.height);
    return c.toDataURL("image/jpeg", 0.85);
  } finally {
    URL.revokeObjectURL(url);
  }
}

async function addImages(files: Iterable<File>) {
  if (!S.server.vision) { toast("Load a vision model first to use images."); return; }
  for (const f of files) {
    if (!f.type.startsWith("image/")) continue;
    if (S.attach.length >= 4) { toast("Up to 4 images per message."); break; }
    try { S.attach.push(await resizeImage(f)); } catch (e) { toast(String(e)); }
  }
  render();
}

function regenerate() {
  const chat = active();
  if (!chat || S.generating) return;
  const last = chat.messages[chat.messages.length - 1];
  if (!last || last.role !== "assistant") return;
  chat.messages.pop();
  void generate(chat);
}

// ═════════════════════════════ engine / models ═════════════════════════════
async function installEngine(variant = "auto") {
  S.engineBusy = true;
  S.engineMsg = "Starting…";
  render();
  const ch = new Channel<Progress>();
  ch.onmessage = (m) => {
    if (m.event === "status") S.engineMsg = m.data.text;
    if (m.event === "bytes") S.engineMsg = `Downloading engine… ${gb(m.data.done / 1048576)} / ${m.data.total ? gb(m.data.total / 1048576) : "?"}`;
    document.querySelectorAll("[data-engine-msg]").forEach((el) => { el.textContent = S.engineMsg; });
  };
  try {
    const tag = await invoke<string>("engine_install", { variant, onEvent: ch });
    toast(`Engine ${tag} installed`);
    S.engineMsg = "";
  } catch (e) {
    S.engineMsg = String(e);
  }
  S.engineBusy = false;
  await refreshEngine();
  render();
}

async function loadModel(file: string) {
  if (!file) return;
  S.loading = true;
  S.loadError = "";
  S.menu = null;
  render();
  try {
    const c = S.cluster;
    // A picture takes hundreds of tokens, so vision models get at least 4096 of context and one slot.
    const isVision = S.installed.find((m) => m.file === file)?.vision ?? false;
    const ctx = isVision ? Math.max(S.settings.ctx, 4096) : S.settings.ctx;
    const parallel = isVision ? 1 : S.settings.parallel;
    S.attach = [];
    S.server = await invoke<Server>("server_start", {
      model: file, ctx, threads: S.settings.threads, parallel,
      workers: c.enabled ? c.workers.filter((w) => !w.off).map((w) => ({ endpoint: w.endpoint, ramMb: w.ramMb })) : [],
      forceShare: c.forceShare,
      api: S.settings.apiEnabled ? { port: S.settings.apiPort, network: S.settings.apiNetwork, key: S.settings.apiKey } : null,
    });
    S.loaded = { ctx, parallel };
    S.settings.lastModel = file;
    S.ctxTokens = 0;
    saveSettings();
  } catch (e) {
    S.loadError = String(e);
    await refreshServer();
  }
  S.loading = false;
  render();
}

async function unloadModel() {
  await invoke("server_stop");
  await refreshServer();
  S.ctxTokens = 0;
  render();
}

type Proj = { url: string; file: string };
async function downloadModel(id: string, name: string, url: string, file: string, proj?: Proj | null) {
  S.downloads.set(id, { name, done: 0, total: 0 });
  render();
  let label = name;
  const ch = new Channel<Progress>();
  ch.onmessage = (m) => {
    if (m.event !== "bytes") return;
    S.downloads.set(id, { name: label, done: m.data.done, total: m.data.total });
    const w = m.data.total ? `${pct(m.data.done, m.data.total)}%` : "30%";
    const txt = `${gb(m.data.done / 1048576)} / ${m.data.total ? gb(m.data.total / 1048576) : "?"}`;
    document.querySelectorAll<HTMLElement>(`[data-bar="${CSS.escape(id)}"]`).forEach((b) => { b.style.width = w; });
    document.querySelectorAll<HTMLElement>(`[data-dl="${CSS.escape(id)}"]`).forEach((b) => { b.textContent = txt; });
  };
  try {
    // A vision model is two files: the model, then the projector that lets it read images.
    if (!S.installed.some((m) => m.file === file)) await invoke("model_download", { id, url, file, onEvent: ch });
    if (proj) {
      label = `${name} — vision part`;
      S.downloads.set(id, { name: label, done: 0, total: 0 });
      render();
      await invoke("model_download", { id, url: proj.url, file: proj.file, onEvent: ch });
    }
    S.downloads.delete(id);
    toast(`${modelLabel(file)} downloaded`);
  } catch (e) {
    const msg = String(e);
    if (msg === "cancelled") S.downloads.delete(id); else S.downloads.set(id, { name, done: 0, total: 0, error: msg });
  }
  await refreshInstalled();
  if (!S.previewModel) S.previewModel = file;
  render();
}

// ═════════════════════════════ cluster ═════════════════════════════
let previewTimer: number | undefined;
function schedulePreview() {
  window.clearTimeout(previewTimer);
  previewTimer = window.setTimeout(() => void refreshPreview(), 120);
}
async function refreshPreview() {
  if (!S.previewModel || !S.installed.some((m) => m.file === S.previewModel)) {
    S.previewModel = S.installed[0]?.file ?? "";
  }
  if (!S.previewModel) { S.preview = null; S.previewErr = ""; if (S.view === "cluster") render(); return; }
  try {
    const workers = S.cluster.enabled ? S.cluster.workers.filter((w) => !w.off).map((w) => ({ endpoint: w.endpoint, ramMb: w.ramMb })) : [];
    S.preview = await invoke<Preview>("cluster_preview", { model: S.previewModel, workers, forceShare: S.cluster.forceShare });
    S.previewErr = "";
  } catch (e) {
    S.preview = null;
    S.previewErr = String(e);
  }
  if (S.view === "cluster") render();
}

async function checkWorkers() {
  // A worker serves one client at a time, so while this PC's loaded model is using
  // it a new connection test would fail. A model split across workers means they are connected.
  const inUse = S.server.running && !!S.server.note && S.server.note.includes("worker");
  await Promise.all(S.cluster.workers.map(async (w) => {
    w.ok = inUse && !w.off ? true : await invoke<boolean>("worker_probe", { endpoint: w.endpoint });
  }));
  if (S.view === "cluster") render();
}

function addWorker() {
  const raw = $<HTMLInputElement>("#c-endpoint").value.trim();
  const ramField = Number($<HTMLInputElement>("#c-ram").value);
  const parsed = parseWorkerInput(raw, ramField);
  if ("error" in parsed) { S.addMsg = parsed.error; render(); return; }
  if (S.cluster.workers.some((w) => w.endpoint === parsed.endpoint)) { S.addMsg = "That PC is already added."; render(); return; }
  const worker: Worker = { endpoint: parsed.endpoint, ramMb: parsed.ramMb };
  S.addMsg = "Checking…";
  const busy = S.server.running && !!S.server.note && S.server.note.includes("worker") && S.cluster.workers.some((x) => x.endpoint === worker.endpoint);
  void (busy ? Promise.resolve(true) : invoke<boolean>("worker_probe", { endpoint: worker.endpoint })).then((ok) => {
    worker.ok = ok;
    S.cluster.workers.push(worker);
    S.cluster.enabled = true;
    saveCluster();
    S.addMsg = ok ? "" : "Added, but it can't be reached right now. Check that the worker is running and the firewall allows it.";
    render();
    schedulePreview();
  });
  render();
}

// ═════════════════════════════ rendering: pieces ═════════════════════════════
function msgHtml(m: Msg, idx: number, last: boolean): string {
  if (m.role === "user") return `<div class="msg user"><div class="bubble">${(m.images ?? []).map((u) => `<img src="${u}" alt="Attached image">`).join("")}${esc(m.content)}</div></div>`;
  const think = m.reasoning && S.settings.thinking !== "hidden"
    ? `<details class="think" ${S.settings.thinking === "open" ? "open" : ""}><summary>${ic("bolt")} Thinking</summary><div>${md(m.reasoning)}</div></details>` : "";
  const body = m.content ? md(m.content) : (S.generating && last ? `<div class="dots"><i></i><i></i><i></i></div>` : "");
  const acts = m.content && !(S.generating && last)
    ? `<div class="acts"><button class="ibtn" data-act="copy-msg" data-i="${idx}" title="Copy">${ic("copy")}</button>${last ? `<button class="ibtn" data-act="regen" title="Try again">${ic("refresh")}</button>` : ""}${m.stats ? `<span class="mstat">${esc(m.stats)}</span>` : ""}</div>` : "";
  return `<div class="msg assistant"><div class="text">${think}${body}</div>${acts}</div>`;
}

function paintLastMessage() {
  const chat = active();
  const list = document.querySelector<HTMLElement>("#messages");
  if (!chat || !list || !list.lastElementChild) return;
  const i = chat.messages.length - 1;
  list.lastElementChild.outerHTML = msgHtml(chat.messages[i], i, true);
  const sc = document.querySelector<HTMLElement>("#scroll");
  if (sc && sc.scrollHeight - sc.scrollTop - sc.clientHeight < 160) sc.scrollTop = sc.scrollHeight;
}

function sidebarHtml(): string {
  const q = S.query.trim().toLowerCase();
  const list = S.chats.filter((c) => !q || c.title.toLowerCase().includes(q) || c.messages.some((m) => m.content.toLowerCase().includes(q)));
  const startOfDay = new Date().setHours(0, 0, 0, 0);
  const groups: [string, Chat[]][] = [["Today", list.filter((c) => c.updated >= startOfDay)], ["Earlier", list.filter((c) => c.updated < startOfDay)]];
  const row = (c: Chat) => S.renamingId === c.id
    ? `<div class="chat-row on"><input id="rename-input" type="text" value="${esc(c.title)}" maxlength="80"></div>`
    : `<div class="chat-row ${c.id === S.activeId && S.view === "chat" ? "on" : ""}"><button class="t" data-chat="${c.id}" title="${esc(c.title)}">${esc(c.title)}</button><button class="x" data-del="${c.id}" title="Delete">${ic("x")}</button></div>`;
  return `<aside class="sidebar">
    <div class="sb-top"><button class="ibtn" data-act="toggle-side" title="Hide sidebar (Ctrl+B)">${ic("sidebar")}</button></div>
    <nav class="sb-nav">
      <button class="nav-item" data-act="new-chat">${ic("plus")}New chat<span class="kbd">Ctrl+N</span></button>
      <button class="nav-item" data-act="search-toggle">${ic("search")}Search<span class="kbd">Ctrl+K</span></button>
      <button class="nav-item ${S.view === "models" ? "on" : ""}" data-view="models">${ic("box")}Models</button>
      <button class="nav-item ${S.view === "cluster" ? "on" : ""}" data-view="cluster">${ic("network")}Cluster${mesh.deviceCount() > 1 ? `<span class="kbd">${mesh.deviceCount()}</span>` : S.worker.running || (S.cluster.enabled && S.cluster.workers.length) ? `<span class="kbd">${S.cluster.workers.length || "●"}</span>` : ""}</button>
    </nav>
    ${S.searchOpen ? `<div class="sb-search"><input id="search-input" type="text" placeholder="Search chats…" value="${esc(S.query)}"></div>` : ""}
    <div class="sb-list">${groups.map(([t, cs]) => cs.length ? `<div class="sb-sec">${t}</div>${cs.map(row).join("")}` : "").join("") || `<div class="sb-sec">No chats found</div>`}</div>
    <div class="sb-foot"><div class="avatar">${ic("cpu").replace('class="i"', 'class="i" style="width:15px;height:15px"')}</div>
      <div class="who"><b>This PC</b><span>${S.hw ? esc(short(S.hw.cpu.replace(/\(R\)|\(TM\)| with .*$/g, ""), 26)) : ""}</span></div>
      <button class="ibtn" data-act="open-settings" title="Settings (Ctrl+,)">${ic("sliders")}</button></div>
  </aside>`;
}

function headerHtml(): string {
  const chat = active();
  const left = S.view === "chat"
    ? `<button class="title" data-menu="chat"><span>${esc(chat?.title ?? "New chat")}</span>${ic("chev")}</button>`
    : `<span class="title" style="cursor:default"><span>${S.view === "models" ? "Models" : "Cluster"}</span></span>`;
  return `<header class="hdr">${S.side ? "" : `<button class="ibtn" data-act="toggle-side" title="Show sidebar (Ctrl+B)">${ic("sidebar")}</button>`}${left}<span class="sp"></span>
    <button class="ibtn ${S.right ? "on" : ""}" data-act="toggle-right" title="Details panel (Ctrl+.)">${ic("panel")}</button></header>`;
}

function stepsHtml(): string {
  const eng = S.engine.installed, hasModel = S.installed.length > 0, run = S.server.running;
  const step = (done: boolean, name: string, sub: string, action: string) =>
    `<div class="step ${done ? "done" : ""}"><span class="ck">${done ? ic("check").replace('class="i"', 'class="i" style="width:12px;height:12px"') : ""}</span><div style="flex:1"><div class="nm">${name}</div><div class="sub">${sub}</div></div>${done ? "" : action}</div>`;
  return `<div class="steps">
    ${step(eng, "Install the engine", S.engineBusy || S.engineMsg ? `<span data-engine-msg>${esc(S.engineMsg)}</span>` : "llama.cpp, from its official GitHub releases (about 60 MB)", `<button class="btn sm" data-act="install-engine" ${S.engineBusy ? "disabled" : ""}>Install</button>`)}
    ${step(hasModel, "Download a model", "A 1B–3B model is a good first choice for this PC", `<button class="btn sm" data-view="models">Browse models</button>`)}
    ${step(run, "Load it and start chatting", S.loading ? "Loading…" : "Pick the model under the message box", hasModel && eng ? `<button class="btn sm" data-menu="model">Choose model</button>` : "")}
  </div>`;
}

const SUGGESTIONS: [string, string][] = [
  ["Explain", "Explain how a CPU cache works, with an everyday analogy."],
  ["Write code", "Write a Python script that renames all files in a folder by date."],
  ["Brainstorm", "Give me 10 name ideas for a small neighborhood coffee shop."],
  ["Summarize", "Summarize the key ideas of the Pareto principle in five bullet points."],
];

function chatBodyHtml(): string {
  const chat = active()!;
  if (!chat.messages.length) {
    const ready = chatViaCluster() || (S.engine.installed && S.server.running);
    return `<div class="col"><div class="hero"><h1>${ready ? "What can I help with?" : "Welcome to LanLM"}</h1><p>${ready ? (chatViaCluster() ? `Running on <b>your cluster</b>. Nothing leaves your network.` : `Running <b>${esc(modelLabel(S.server.model ?? ""))}</b> on your own PC. Nothing leaves it.`) : "Run open models privately on your own PC, or across several PCs."}</p></div>
      ${ready && S.server.vision ? `<p class="hint" style="text-align:center;margin:-4px 0 0">${ic("image")} This model can see images: attach one with <b>+</b>, or paste or drop it into the message box.</p>` : ""}
      ${ready ? `<div class="sugs">${SUGGESTIONS.map(([k, t]) => `<button class="sug" data-act="sug" data-text="${esc(t)}"><small>${k}</small>${esc(t)}</button>`).join("")}</div>` : stepsHtml()}</div><div id="messages" hidden></div>`;
  }
  return `<div class="col msgs" id="messages">${chat.messages.map((m, i) => msgHtml(m, i, i === chat.messages.length - 1)).join("")}</div>`;
}

function composerHtml(): string {
  const viaCluster = chatViaCluster();
  const run = S.server.running || viaCluster;
  const dot = S.loading ? "busy" : run ? "on" : "";
  const label = S.loading ? "Loading model…" : viaCluster ? "Whole cluster" : run ? modelLabel(S.server.model ?? "") : "Choose a model";
  const meta = viaCluster ? `${mesh.deviceCount()} PCs${S.lastTps ? ` · ${S.lastTps.toFixed(1)} tok/s` : ""}` : run ? `${S.loaded.ctx / S.loaded.parallel | 0} ctx${S.loaded.parallel > 1 ? ` · ${S.loaded.parallel} slots` : ""}${S.server.note ? " · cluster" : ""}${S.lastTps ? ` · ${S.lastTps.toFixed(1)} tok/s` : ""}` : "";
  return `<div class="composer"><div class="col">
    ${S.loadError ? `<p class="error">${esc(S.loadError)}</p>` : ""}
    ${S.queue.length ? `<div class="queued">${S.queue.length} message${S.queue.length > 1 ? "s" : ""} queued — will send when this reply finishes</div>` : ""}
    <div class="cbox" id="cbox">
      ${S.attach.length ? `<div class="thumbs">${S.attach.map((u, i) => `<div class="thumb"><img src="${u}" alt="Attached image ${i + 1}"><button data-act="rm-attach" data-i="${i}" title="Remove">${ic("x")}</button></div>`).join("")}</div>` : ""}
      <input type="file" id="file-input" accept="image/*" multiple hidden>
      <div class="row">
      <button class="ibtn" data-menu="plus" title="More">${ic("plus")}</button>
      <textarea id="input" rows="1" placeholder="${S.generating ? "Reply is generating… press Enter to queue your next message" : S.server.vision ? "Message your model… (paste or drop images here)" : "Message your model…"}"></textarea>
      ${S.generating ? `<button class="send" data-act="stop" title="Stop">${ic("stop")}</button>` : `<button class="send" data-act="send" title="Send (Enter)">${ic("up")}</button>`}
    </div>
    <div class="cfoot"><button class="chip" data-menu="model"><span class="dot ${dot}"></span>${esc(label)}${S.server.vision ? ` <span class="badge good">Vision</span>` : ""}${ic("chev")}</button><span>${esc(meta)}</span></div></div>
  </div></div>`;
}

function chatView(): string {
  return `<div class="scroll" id="scroll">${chatBodyHtml()}</div>${composerHtml()}`;
}

function modelsView(): string {
  const have = new Map(S.installed.map((m) => [m.file, m]));
  const items = S.catalog.filter((c) => S.filter === "all" || (S.filter === "fits" && fitsPc(c.sizeMb + c.projMb)) || (S.filter === "installed" && have.has(c.file)) || (S.filter === "vision" && c.vision));
  const row = (c: CatalogItem) => {
    const d = S.downloads.get(c.id);
    const inst = have.get(c.file) && (!c.vision || have.get(c.file)!.vision) ? have.get(c.file) : undefined;
    const action = inst ? `<button class="btn light sm" data-act="load-model" data-file="${esc(c.file)}" ${S.loading ? "disabled" : ""}>${S.server.model === c.file ? "Loaded" : "Load"}</button>`
      : d && !d.error ? `<button class="btn light sm" data-act="dl-cancel" data-id="${esc(c.id)}">Cancel</button>`
      : `<button class="btn sm" data-act="dl" data-id="${esc(c.id)}">${ic("download")}Download</button>`;
    const prog = d && !d.error ? `<div class="meter bar"><i data-bar="${esc(c.id)}" style="width:${pct(d.done, d.total)}%"></i></div><div class="ds" data-dl="${esc(c.id)}"></div>` : "";
    return `<div class="item"><div class="ic">${ic("box")}</div><div class="body"><div class="nm">${esc(c.name)} ${c.vision ? `<span class="badge good">${ic("image").replace('class="i"', 'class="i" style="width:12px;height:12px"')}Vision</span>` : ""} ${fitBadge(c.sizeMb + c.projMb)}</div><div class="ds">${esc(c.desc)} · ${gb(c.sizeMb + c.projMb)}${c.vision ? " (model + image part)" : ""}</div>${prog}${d?.error ? `<div class="error">${esc(d.error)}</div>` : ""}</div>${action}</div>`;
  };
  const known = new Set(S.catalog.map((c) => c.file));
  const others = S.installed.filter((m) => !known.has(m.file));
  const otherRow = (m: Installed) => `<div class="item"><div class="ic">${ic("box")}</div><div class="body"><div class="nm">${esc(m.file)}</div><div class="ds">${gb(m.sizeMb)} · added by you</div></div>
    <button class="btn light sm" data-act="load-model" data-file="${esc(m.file)}">${S.server.model === m.file ? "Loaded" : "Load"}</button><button class="ibtn" data-act="model-delete" data-file="${esc(m.file)}" title="Delete">${ic("trash")}</button></div>`;
  const dls = [...S.downloads.entries()].filter(([id]) => id.startsWith("custom-"));
  return `<div class="scroll"><div class="col wide page">
    <div><h2>Models</h2><p class="lead">Download a model once and it runs on this PC, offline. Any GGUF model that llama.cpp supports works: pick one below or add your own link.</p></div>
    <div class="filters"><button class="pillbtn ${S.filter === "all" ? "on" : ""}" data-act="filter" data-f="all">All</button>
      <button class="pillbtn ${S.filter === "fits" ? "on" : ""}" data-act="filter" data-f="fits">Fits this PC</button>
      <button class="pillbtn ${S.filter === "vision" ? "on" : ""}" data-act="filter" data-f="vision">Vision</button>
      <button class="pillbtn ${S.filter === "installed" ? "on" : ""}" data-act="filter" data-f="installed">Downloaded</button>
      <span style="flex:1"></span><button class="btn light sm" data-act="open-folder">${ic("link")}Open models folder</button></div>
    <div class="list">${items.map(row).join("") || `<div class="item"><div class="ds">Nothing matches this filter.</div></div>`}</div>
    <div class="card"><div class="ch">Add any GGUF model</div><div class="cb"><p class="hint" style="margin:0 0 10px">Paste a direct https link to a .gguf file (for example from Hugging Face), or copy files into the models folder.</p>
      <div class="row-inline"><input type="text" id="custom-url" placeholder="https://huggingface.co/…/resolve/main/model-Q4_K_M.gguf" style="flex:1;min-width:260px"><button class="btn" data-act="custom-dl">${ic("download")}Download</button></div>
      <div class="row-inline" style="margin-top:8px"><input type="text" id="custom-proj" placeholder="Optional: link to its vision file (mmproj-….gguf) for image models" style="flex:1;min-width:260px"></div>
      <div class="error" id="custom-status"></div>${dls.map(([id, d]) => `<div class="ds" style="margin-top:8px">${esc(d.name)} <span data-dl="${esc(id)}"></span></div><div class="meter"><i data-bar="${esc(id)}" style="width:${pct(d.done, d.total)}%"></i></div>${d.error ? `<div class="error">${esc(d.error)}</div>` : ""}`).join("")}</div></div>
    ${others.length ? `<div><h3 style="margin:0 0 10px">Your own models</h3><div class="list">${others.map(otherRow).join("")}</div></div>` : ""}
  </div></div>`;
}

// ───────── cluster page ─────────
function node(x: number, y: number, w: number, h: number, color: string, title: string, sub: string, cap: number, used: number, line: string, badge: string): string {
  return `<g><rect class="node-box" x="${x}" y="${y}" width="${w}" height="${h}" rx="14"/><rect x="${x}" y="${y}" width="6" height="${h}" rx="3" fill="${color}"/>
    <text x="${x + 20}" y="${y + 26}" font-size="14" font-weight="600">${esc(short(title, 24))}</text>
    <text class="mut" x="${x + 20}" y="${y + 45}" font-size="11.5">${esc(sub)}</text>
    <rect x="${x + 20}" y="${y + 55}" width="${w - 40}" height="7" rx="3.5" fill="var(--active)"/>
    <rect x="${x + 20}" y="${y + 55}" width="${Math.max(0, ((w - 40) * pct(used, cap)) / 100)}" height="7" rx="3.5" fill="${color}"/>
    <text x="${x + 20}" y="${y + 80}" font-size="12" font-weight="600">${esc(line)}</text>${badge ? `<text class="mut" x="${x + w - 16}" y="${y + 80}" font-size="11" text-anchor="end">${esc(badge)}</text>` : ""}</g>`;
}

function topoSvg(): string {
  const ws = S.cluster.workers, pv = S.preview, hw = S.hw!;
  const W = 820, nw = 240, nh = 94, gap = 16;
  const rows = Math.max(1, ws.length), H = rows * (nh + gap) + gap, mcy = H / 2;
  const m = pv?.devices[0];
  const act = ws.filter((x) => !x.off);
  let g = "";
  const mx = 28, wx = W - 28 - nw;
  if (!ws.length) {
    g += `<path class="link-line off" d="M ${mx + nw} ${mcy} C ${W / 2} ${mcy}, ${W / 2} ${mcy}, ${wx} ${mcy}"/>`;
    g += `<rect x="${wx}" y="${mcy - nh / 2}" width="${nw}" height="${nh}" rx="14" fill="none" stroke="var(--border-strong)" stroke-width="1.5" stroke-dasharray="6 5"/>
      <text x="${wx + nw / 2}" y="${mcy - 4}" font-size="13" font-weight="600" text-anchor="middle">No other PCs yet</text><text class="mut" x="${wx + nw / 2}" y="${mcy + 16}" font-size="11.5" text-anchor="middle">Add one below to share bigger models</text>`;
  }
  ws.forEach((w, i) => {
    const cy = gap + i * (nh + gap) + nh / 2;
    const ai = act.indexOf(w);
    const d = S.cluster.enabled && ai >= 0 ? pv?.devices[ai + 1] : undefined;
    const live = S.generating && w.ok !== false && pv?.usesWorkers && ai >= 0;
    const cls = w.off || w.ok === false ? "off" : live ? "live" : "";
    g += `<path class="link-line ${cls}" d="M ${mx + nw} ${mcy} C ${W / 2} ${mcy}, ${W / 2} ${cy}, ${wx} ${cy}"/>`;
    const cap = lendableMb(w.ramMb);
    g += node(wx, cy - nh / 2, nw, nh, devColor(i + 1), w.endpoint, `lends ${gb(cap)} of ${gb(w.ramMb)} free`, cap, d?.mb ?? 0,
      d ? `${d.layers} layer${d.layers === 1 ? "" : "s"} · ${gb(d.mb)}` : w.off || !S.cluster.enabled ? "turned off" : "no layers", w.ok === undefined ? "not checked" : w.ok ? "● reachable" : "● unreachable");
  });
  const mcap = m?.capMb ?? hw.availRamMb * 0.7;
  g += node(mx, mcy - nh / 2, nw, nh, devColor(0), "This PC (master)", `${hw.physicalCores} cores · ${gb(mcap)} usable`, mcap, m?.mb ?? 0,
    m && pv ? `${m.layers} of ${pv.nLayer} layers · ${gb(m.mb)}` : "Pick a model", "runs the chat");
  return `<svg viewBox="0 0 ${W} ${H}" role="img" aria-label="Cluster graph">${g}</svg>`;
}

function layerMapHtml(pv: Preview): string {
  const cells: string[] = [];
  let n = 0;
  pv.devices.forEach((d, di) => {
    for (let k = 0; k < d.layers; k++) { n++; cells.push(`<i style="background:${devColor(di)}" title="Layer ${n} → ${esc(d.name)}">${n}</i>`); }
  });
  cells.push(`<i class="out" title="Output layer stays on this PC (avoids sending a large vector over the network each token)">out</i>`);
  const legend = pv.devices.map((d, di) => `<span><b style="background:${devColor(di)}"></b>${esc(short(d.name, 22))} · ${d.layers} layers</span>`).join("");
  return `<div class="layermap">${cells.join("")}</div><div class="legend">${legend}</div>`;
}

function gaugeHtml(pv: Preview): string {
  const scale = Math.max(pv.needMb, ...pv.devices.map((d) => d.capMb)) * 1.05;
  return `<div class="gauge">${pv.devices.map((d, di) => `<div class="grow"><span class="nm">${esc(short(d.name, 20))}</span>
    <div class="gbar" title="${gb(d.mb)} of ${gb(d.capMb)}"><i style="width:${pct(d.capMb, scale)}%;background:${devColor(di)}"></i><b style="width:${pct(d.mb, scale)}%;background:${devColor(di)}"></b></div>
    <span class="gcap">${gb(d.mb)} / ${gb(d.capMb)}</span></div>`).join("")}</div>`;
}

function fitTable(): string {
  const hw = S.hw!;
  const master = hw.availRamMb * 0.7;
  const remote = S.cluster.workers.filter((w) => !w.off).reduce((s, w) => s + lendableMb(w.ramMb), 0);
  const seen = new Set<string>();
  const rows: { name: string; mb: number }[] = [];
  for (const c of S.catalog) { rows.push({ name: c.name, mb: c.sizeMb }); seen.add(c.file); }
  for (const m of S.installed) if (!seen.has(m.file)) rows.push({ name: modelLabel(m.file), mb: m.sizeMb });
  const verdict = (cap: number, need: number) => need <= cap ? `<span class="badge good">Fits</span>` : need <= cap * 1.15 ? `<span class="badge warn">Tight</span>` : `<span class="badge bad">No</span>`;
  return `<div class="list">${rows.map((r) => { const need = r.mb * 1.1; return `<div class="item"><div class="body"><div class="nm">${esc(r.name)}</div><div class="ds">${gb(r.mb)} on disk · needs about ${gb(need)}</div></div>
    <div style="display:flex;gap:18px;align-items:center"><span class="ds" style="width:70px;text-align:center">This PC ${verdict(master, need)}</span><span class="ds" style="width:96px;text-align:center">Cluster ${remote > 0 ? verdict(master + remote, need) : `<span class="badge">no PCs</span>`}</span></div></div>`; }).join("")}</div>`;
}

/** The older manual way to add a PC by address, kept under "More" on the cluster page. */
function legacyClusterHtml(): string {
  const c = S.cluster, w = S.worker, pv = S.preview, hw = S.hw!;
  const act = c.workers.filter((x) => !x.off);
  const opts = S.installed.map((m) => `<option value="${esc(m.file)}" ${m.file === S.previewModel ? "selected" : ""}>${esc(modelLabel(m.file))} (${gb(m.sizeMb)})</option>`).join("");
  const totalCap = (pv?.devices ?? []).reduce((s, d) => s + d.capMb, 0) || hw.availRamMb * 0.7;
  const verdict = !pv ? "" : pv.error ? `<span class="badge bad">Too big for this cluster</span>` : pv.usesWorkers ? `<span class="badge warn">Shared across PCs</span>` : `<span class="badge good">Runs on this PC alone</span>`;
  const status = (x: Worker) => `<span class="badge ${x.ok === undefined ? "" : x.ok ? "good" : "bad"}">${x.ok === undefined ? "not checked" : x.ok ? "● reachable" : "● unreachable"}</span>`;
  const msgCls = /^(Use|Enter|That|Added)/.test(S.addMsg) ? "error" : "hint";

  const thisPc = `<div class="item"><span class="avatar" style="background:${devColor(0)}">${ic("cpu").replace('class="i"', 'class="i" style="width:15px;height:15px"')}</span>
    <div class="body"><div class="nm">This PC <span class="badge">master</span>${w.running ? ` <span class="badge good">● also lending</span>` : ""}</div>
      <div class="ds">${esc(short(hw.cpu.replace(/\(R\)|\(TM\)| with .*$/g, ""), 34))} · about ${gb(hw.availRamMb * 0.7)} usable · runs the chat</div></div></div>`;
  const workerRows = c.workers.map((x, i) => `<div class="item" style="${x.off ? "opacity:.6" : ""}"><span class="avatar" style="background:${devColor(i + 1)}">${i + 1}</span>
    <div class="body"><div class="nm">${esc(x.endpoint)} ${status(x)}</div>
      <div class="ds row-inline" style="gap:6px">Free RAM there <input type="number" class="mini" data-ram-i="${i}" value="${(x.ramMb / 1024).toFixed(1)}" min="0.5" step="0.5"> GB <span>· lends about ${gb(lendableMb(x.ramMb))}</span></div></div>
    <label class="switch" title="${x.off ? "Turned off — not used" : "Use this PC"}"><input type="checkbox" data-wtoggle="${i}" ${x.off ? "" : "checked"}><i></i></label>
    <button class="ibtn" data-act="c-rm" data-i="${i}" title="Remove">${ic("trash")}</button></div>`).join("");

  return `<div class="legacy">
    <p class="hint" style="margin:0 0 14px">The cluster above is the better way. This older method links PCs by address with no password: every PC you add is a plain worker for a model loaded on this PC, and anyone on the network could use a worker. Only use it on a network you trust.</p>

    <div class="two">
      <div class="card primary"><div class="ch"><span>${ic("plus")} Add another PC</span></div><div class="cb">
        <ol class="steps2"><li>On the other PC, open LanLM → <b>Cluster</b> → <b>Start worker</b>.</li><li>Paste the pairing code it shows here.</li></ol>
        <div class="row-inline"><input type="text" id="c-endpoint" placeholder="192.168.1.20:50052@5120" style="flex:1;min-width:170px" autocomplete="off">
          <button class="btn light" data-act="c-paste" title="Paste from clipboard">${ic("copy")}Paste</button></div>
        <div class="row-inline" style="margin-top:10px;justify-content:space-between"><label class="row-inline" id="c-ram-wrap">Free RAM on that PC <input type="number" id="c-ram" value="4" min="0.5" step="0.5" class="mini"> GB</label>
          <button class="btn" data-act="c-add">${ic("plus")}Add PC</button></div>
        <div class="${msgCls}" style="margin-top:8px">${esc(S.addMsg)}</div></div></div>

      <div class="card primary"><div class="ch"><span>${ic("link")} Lend this PC</span>${w.running ? `<span class="badge good">● running</span>` : ""}</div><div class="cb">
        <p class="hint" style="margin:0 0 10px">Let another LanLM use this PC's memory. A worker has no password, so only turn it on on a network you trust.</p>
        <div class="row-inline" style="margin-bottom:12px"><label class="row-inline">Port <input type="number" id="w-port" value="${c.workerPort}" min="1024" max="65535" class="mini" style="width:84px" ${w.running ? "disabled" : ""}></label>
          <label class="row-inline"><span class="switch"><input type="checkbox" id="w-lan" ${c.workerLan ? "checked" : ""} ${w.running ? "disabled" : ""}><i></i></span>Allow other PCs</label>
          <span style="flex:1"></span>${w.running ? `<button class="btn danger" data-act="worker-stop">Stop worker</button>` : `<button class="btn" data-act="worker-start" ${S.engine.installed ? "" : "disabled"}>Start worker</button>`}</div>
        ${w.running && w.pairing ? `<div class="pair"><div class="hint">Pairing code — paste it on the other PC:</div><div class="row-inline" style="margin-top:6px;justify-content:space-between"><code>${esc(w.pairing)}</code><button class="btn light sm" data-act="pair-copy">${ic("copy")}Copy</button></div></div>` : ""}
        <div class="error" style="margin-top:6px">${esc(S.clusterMsg)}</div></div></div>
    </div>

    <div class="card"><div class="ch"><span>Your PCs <span class="badge">${1 + act.length} in use</span></span><button class="btn light sm" data-act="c-check">${ic("refresh")}Check connections</button></div>
      <div class="cb" style="padding:12px 0 0"><div class="list" style="border:0;border-radius:0 0 14px 14px;border-top:1px solid var(--border)">${thisPc}${workerRows || `<div class="item"><div class="ds">No other PCs yet. Add one above to run bigger models.</div></div>`}</div></div></div>

    <div class="card"><div class="ch"><span>Preview</span><select id="pv-model" style="max-width:260px" ${S.installed.length ? "" : "disabled"}>${opts || "<option>No models downloaded</option>"}</select></div>
      <div class="cb">
        <div class="row-inline" style="margin-bottom:14px;justify-content:space-between">
          <label class="row-inline"><span class="switch"><input type="checkbox" id="c-enabled" ${c.enabled ? "checked" : ""}><i></i></span>Use other PCs when loading a model</label>
          <label class="row-inline"><span class="switch"><input type="checkbox" id="c-force" ${c.forceShare ? "checked" : ""}><i></i></span>Share even when the model fits here</label></div>
        <div class="topo">${topoSvg()}</div>
        ${S.previewErr ? `<p class="error">${esc(S.previewErr)}</p>` : ""}
        ${pv ? `<div class="summary" style="margin-top:14px">
            <div class="stat"><small>Model</small><b>${gb(pv.fileMb)}</b></div><div class="stat"><small>Needs (est.)</small><b>${gb(pv.needMb)}</b></div>
            <div class="stat"><small>Cluster can lend</small><b>${gb(totalCap)}</b></div><div class="stat"><small>Layers</small><b>${pv.nLayer}</b></div><div class="stat"><small>Result</small><div style="margin-top:4px">${verdict}</div></div></div>
          ${pv.error ? `<p class="error" style="margin-top:10px">${esc(pv.error)}</p>` : pv.note ? `<p class="hint" style="margin:10px 0 0">${esc(pv.note)}</p>` : ""}`
          : `<p class="hint" style="margin-top:12px">${S.installed.length ? "" : "Download a model on the Models page to preview how it would be split."}</p>`}
        <p class="hint" style="margin:12px 0 0">The cluster is used the next time you load a model.</p>
      </div></div>

    ${pv ? `<div class="two"><div class="card"><div class="ch">Where each layer runs</div><div class="cb">${layerMapHtml(pv)}</div></div>
      <div class="card"><div class="ch">Memory used on each PC</div><div class="cb">${gaugeHtml(pv)}</div></div></div>` : ""}

    <div><h3 style="margin:0 0 4px">What fits</h3><p class="hint" style="margin:0 0 10px">Estimated from free memory right now. Adding PCs raises what the cluster can run.</p>${fitTable()}</div>
  </div>`;
}

function clusterView(): string {
  return mesh.meshView();
}

// ───────── right panel ─────────
function rightHtml(): string {
  const hw = S.hw!, run = S.server.running;
  const perSlot = S.loaded.ctx / Math.max(1, S.loaded.parallel);
  const ctxPct = pct(S.ctxTokens, perSlot);
  const tasks: string[] = [];
  if (S.engineBusy) tasks.push(`<div class="task"><span class="spin"></span><span class="n" data-engine-msg>${esc(S.engineMsg)}</span></div>`);
  if (S.loading) tasks.push(`<div class="task"><span class="spin"></span><span class="n">Loading model…</span></div>`);
  for (const [id, d] of S.downloads) if (!d.error) tasks.push(`<div class="task"><span class="spin"></span><span class="n" title="${esc(d.name)}">${esc(short(d.name, 22))}</span><span class="v" style="margin-left:auto;color:var(--muted)" data-dl="${esc(id)}"></span><button class="ibtn" data-act="dl-cancel" data-id="${esc(id)}" style="width:22px;height:22px">${ic("x")}</button></div>`);
  if (S.worker.running) tasks.push(`<div class="task"><span class="badge good">●</span><span class="n">Worker lending this PC</span><button class="ibtn" data-act="worker-stop" style="margin-left:auto;width:22px;height:22px" title="Stop">${ic("stop")}</button></div>`);
  if (run && S.server.apiUrl) tasks.push(`<div class="task"><span class="badge good">●</span><span class="n" title="${esc(S.server.apiUrl)}">API ${esc(S.server.apiUrl.replace(/^https?:\/\//, ""))}</span></div>`);
  const models = S.installed.slice(0, 6).map((m) => `<button class="rrow nav-item" style="padding:5px 8px" data-act="load-model" data-file="${esc(m.file)}">${ic("box")}<span class="n">${esc(modelLabel(m.file))}</span><span class="v">${S.server.model === m.file ? "loaded" : gb(m.sizeMb)}</span></button>`).join("");
  const workers = S.cluster.workers.map((w, i) => `<div class="rrow"><span class="badge" style="background:${devColor(i + 1)};color:#fff;padding:0 6px">${i + 1}</span><span class="n">${esc(w.endpoint)}</span><span class="v">${w.ok === false ? "offline" : gb(lendableMb(w.ramMb))}</span></div>`).join("");
  return `<aside class="rpanel">
    <div class="rsec"><h4>This PC</h4>
      <div class="rrow">${ic("cpu")}<span class="n">${esc(short(hw.cpu.replace(/\(R\)|\(TM\)| with .*$/g, ""), 30))}</span></div>
      <div class="rrow"><span class="n">Memory</span><span class="v">${gb(hw.availRamMb)} free of ${gb(hw.totalRamMb)}</span></div><div class="meter ${pct(hw.totalRamMb - hw.availRamMb, hw.totalRamMb) > 85 ? "warn" : ""}"><i style="width:${pct(hw.totalRamMb - hw.availRamMb, hw.totalRamMb)}%"></i></div>
      <div class="rrow"><span class="n">Threads</span><span class="v">${S.settings.threads} of ${hw.logicalCores}</span></div></div>
    <div class="rsec"><h4>Context ${run ? `<span class="hint">${S.ctxTokens.toLocaleString()} / ${Math.round(perSlot).toLocaleString()}</span>` : ""}</h4>
      ${run ? `<div class="meter ${ctxPct > 90 ? "bad" : ctxPct > 70 ? "warn" : ""}"><i style="width:${ctxPct}%"></i></div><p class="hint" style="margin:8px 0 0">${ctxPct.toFixed(0)}% of this chat's memory used${S.loaded.parallel > 1 ? ` (${S.loaded.parallel} slots share the context)` : ""}.</p>
      <div class="rrow" style="margin-top:6px"><span class="n">${esc(modelLabel(S.server.model ?? ""))}</span><button class="btn light sm" style="margin-left:auto" data-act="unload">Unload</button></div>` : `<p class="hint" style="margin:0">No model loaded.</p>`}</div>
    <div class="rsec"><h4>Cluster <button class="link" data-view="cluster">Open</button></h4>
      ${workers || `<p class="hint" style="margin:0">Just this PC. Add others to run bigger models.</p>`}
      ${S.server.note ? `<p class="hint" style="margin:8px 0 0">${esc(S.server.note)}</p>` : ""}</div>
    <div class="rsec"><h4>Background tasks</h4>${tasks.join("") || `<p class="hint" style="margin:0">Nothing running.</p>`}</div>
    <div class="rsec"><h4>Models <button class="link" data-view="models">Browse</button></h4>${models || `<p class="hint" style="margin:0">None downloaded yet.</p>`}</div>
    <div class="rsec"><details id="log-details"><summary style="cursor:pointer;font-weight:600">Engine log</summary><div class="logbox" id="logbox" style="margin-top:10px">${esc(S.logs.join("\n"))}</div></details></div>
  </aside>`;
}

// ───────── settings modal / menus ─────────
const row = (label: string, desc: string, control: string) => `<div class="srow"><div class="l"><b>${label}</b><span>${desc}</span></div><div class="r">${control}</div></div>`;
const sw = (id: string, on: boolean) => `<span class="switch"><input type="checkbox" id="${id}" ${on ? "checked" : ""}><i></i></span>`;
const segment = (act: string, cur: string | number, opts: [string | number, string][]) => `<div class="seg">${opts.map(([v, l]) => `<button class="${String(v) === String(cur) ? "on" : ""}" data-act="${act}" data-v="${v}">${l}</button>`).join("")}</div>`;

function modalHtml(): string {
  if (!S.modal) return "";
  const s = S.settings, hw = S.hw!;
  const tabs: [Tab, string, string][] = [["general", "General", "chat"], ["engine", "Model & engine", "cpu"], ["api", "API server", "server"], ["appearance", "Appearance", "image"], ["about", "Shortcuts & about", "bolt"]];
  let body: string;
  if (S.modal === "general") {
    body = `<h3>Chat</h3>
      ${row("System prompt", "Instructions sent with every chat.", `<textarea id="s-system" rows="4">${esc(s.system)}</textarea>`)}
      ${row("Temperature", "Higher is more creative, lower is more focused.", `<input type="number" id="s-temp" value="${s.temperature}" min="0" max="2" step="0.1">`)}
      ${row("Maximum reply length", "Longest answer, in tokens.", `<input type="number" id="s-max" value="${s.maxTokens}" min="16" step="64">`)}
      ${row("Show model thinking", "For models that think before answering.", segment("set-think", s.thinking, [["collapsed", "Collapsed"], ["open", "Open"], ["hidden", "Hidden"]]))}
      ${row("Clear all chats", "Deletes every conversation saved on this PC.", `<button class="btn danger" data-act="clear-chats">Delete all</button>`)}`;
  } else if (S.modal === "engine") {
    body = `<h3>Model & engine</h3>
      ${row("Context size", "Tokens the model remembers. More uses more memory. Applies on the next load.", `<input type="number" id="s-ctx" value="${s.ctx}" min="512" step="512">`)}
      ${row("CPU threads", `This PC has ${hw.physicalCores} cores (${hw.logicalCores} threads). Physical cores usually run fastest.`, `<input type="number" id="s-threads" value="${s.threads}" min="1" max="${hw.logicalCores}">`)}
      ${row("Parallel slots", "Answer several requests at once (they are batched together). The context is shared between slots.", `<input type="number" id="s-parallel" value="${s.parallel}" min="1" max="8">`)}
      ${row("Engine", S.engine.installed ? `llama.cpp ${esc(S.engine.version ?? "")}` : "Not installed yet.", `<button class="btn light" data-act="install-engine" ${S.engineBusy ? "disabled" : ""}>${S.engine.installed ? "Update" : "Install"}</button>`)}
      <div class="hint" data-engine-msg>${esc(S.engineMsg)}</div>`;
  } else if (S.modal === "api") {
    body = `<h3>API server</h3>
      ${row("Share as an API", "Lets other apps (Open WebUI, coding tools, scripts, the OpenAI SDKs) use the loaded model. Applies on the next load.", sw("s-api-enabled", s.apiEnabled))}
      ${row("Port", "Where the API listens.", `<input type="number" id="s-api-port" value="${s.apiPort}" min="1024" max="65535">`)}
      ${row("Allow other devices on my network", "Otherwise only this PC can reach it. A key is required.", sw("s-api-network", s.apiNetwork))}
      ${row("API key", "Without a key, anything on this PC can use the API.", `<input type="text" id="s-api-key" value="${esc(s.apiKey)}" placeholder="none"><button class="btn light sm" data-act="api-genkey">Create</button><button class="ibtn" data-act="api-copy-key" title="Copy">${ic("copy")}</button>`)}
      ${S.server.running && S.server.apiUrl ? row("Address now", `Send requests to <b>${esc(S.server.apiUrl)}/chat/completions</b> with the header <code>Authorization: Bearer &lt;key&gt;</code>.`, `<span class="badge good">● live</span>`) : ""}`;
  } else if (S.modal === "appearance") {
    body = `<h3>Appearance</h3>
      ${row("Theme", "Match your system, or choose.", segment("set-theme", s.theme, [["system", "System"], ["light", "Light"], ["dark", "Dark"]]))}
      ${row("Text size", "Scales the whole app.", segment("set-scale", s.scale, [[0.93, "Small"], [1, "Default"], [1.1, "Large"]]))}`;
  } else {
    const k = (a: string, b: string) => `<div class="srow"><div class="l"><b>${a}</b></div><div class="r"><span class="kbd2">${b}</span></div></div>`;
    body = `<h3>Shortcuts</h3>${k("New chat", "Ctrl + N")}${k("Search chats", "Ctrl + K")}${k("Toggle sidebar", "Ctrl + B")}${k("Toggle details panel", "Ctrl + .")}${k("Settings", "Ctrl + ,")}${k("Send / queue a message", "Enter")}${k("New line", "Shift + Enter")}
      <h3 style="margin-top:22px">About</h3><p class="hint">LanLM runs open models on your own PC with llama.cpp. Chats and settings stay on this computer. Engine: ${S.engine.installed ? `llama.cpp ${esc(S.engine.version ?? "")}` : "not installed"}.</p>`;
  }
  return `<div class="modal-back" data-act="close-modal"><div class="modal" role="dialog" aria-label="Settings"><div class="modal-h"><span>Settings</span><button class="ibtn" data-act="close-modal">${ic("x")}</button></div>
    <div class="modal-b"><nav class="modal-nav">${tabs.map(([id, l, i]) => `<button class="nav-item ${S.modal === id ? "on" : ""}" data-act="modal-tab" data-tab="${id}">${ic(i)}${l}</button>`).join("")}</nav><div class="modal-c">${body}</div></div></div></div>`;
}

function menuHtml(): string {
  const m = S.menu;
  if (!m) return "";
  const pos = `left:${Math.max(8, Math.min(m.left, innerWidth - 250))}px;${m.top !== undefined ? `top:${m.top}px` : `bottom:${m.bottom}px`}`;
  let items: string;
  if (m.kind === "chat") {
    items = `<button data-act="rename-chat">${ic("pencil")}Rename</button><button data-act="copy-chat">${ic("copy")}Copy chat as Markdown</button><div class="sep"></div><button data-act="del-chat" style="color:var(--bad)">${ic("trash")}Delete chat</button>`;
  } else if (m.kind === "model") {
    const clusterItem = mesh.deviceCount() > 1 ? `<button data-act="chat-cluster">${S.useCluster ? ic("check") : `<span style="width:16px"></span>`}<span>Use the whole cluster</span><span class="sub">${mesh.deviceCount()} PCs</span></button><div class="sep"></div>` : "";
    items = `${clusterItem}<div class="lab">Models on this PC</div>${S.installed.map((x) => `<button data-act="load-model" data-file="${esc(x.file)}">${S.server.model === x.file ? ic("check") : `<span style="width:16px"></span>`}<span>${esc(modelLabel(x.file))}</span>${x.vision ? `<span class="badge good" style="margin-left:6px">Vision</span>` : ""}<span class="sub">${gb(x.sizeMb)}</span></button>`).join("") || `<div class="lab">None downloaded yet</div>`}
      <div class="sep"></div><button data-view="models">${ic("download")}Download more models…</button>${S.server.running ? `<button data-act="unload">${ic("stop")}Unload model</button>` : ""}`;
  } else {
    items = `<button data-act="attach-image" ${S.server.vision ? "" : "disabled"}>${ic("image")}Attach image${S.server.vision ? "" : `<span class="sub">needs a vision model</span>`}</button><button data-act="open-settings-general">${ic("sliders")}Edit system prompt…</button><button data-view="models">${ic("box")}Browse models…</button><button data-view="cluster">${ic("network")}Cluster…</button>`;
  }
  return `<div class="menu" style="${pos}">${items}</div>`;
}

// ═════════════════════════════ render ═════════════════════════════
function ensureOverlay(): HTMLElement {
  let o = document.querySelector<HTMLElement>("#overlay");
  if (!o) { o = document.createElement("div"); o.id = "overlay"; document.body.appendChild(o); }
  return o;
}
function renderOverlay() { ensureOverlay().innerHTML = menuHtml() + modalHtml(); }

function render() {
  const keep = document.querySelector<HTMLTextAreaElement>("#input")?.value ?? "";
  const hadFocus = document.activeElement?.id === "input";
  const logOpen = (document.querySelector("#log-details") as HTMLDetailsElement | null)?.open ?? false;
  const main = S.view === "chat" ? chatView() : S.view === "models" ? modelsView() : clusterView();
  $("#app").innerHTML = `<div class="app ${S.side ? "" : "no-side"} ${S.right ? "" : "no-right"}">${sidebarHtml()}<section class="main">${headerHtml()}${main}</section>${rightHtml()}</div>`;
  if (logOpen) (document.querySelector("#log-details") as HTMLDetailsElement).open = true;
  if (S.view === "chat") {
    const input = document.querySelector<HTMLTextAreaElement>("#input");
    if (input) { input.value = keep; if (hadFocus || !S.modal) input.focus(); }
    const sc = document.querySelector<HTMLElement>("#scroll");
    if (sc) sc.scrollTop = sc.scrollHeight;
  }
  if (S.renamingId) { const r = document.querySelector<HTMLInputElement>("#rename-input"); r?.focus(); r?.select(); }
  if (S.searchOpen && !S.renamingId) document.querySelector<HTMLInputElement>("#search-input")?.focus();
  renderOverlay();
}

// ═════════════════════════════ events ═════════════════════════════
function openMenu(kind: Menu["kind"], el: HTMLElement) {
  if (S.menu?.kind === kind) { S.menu = null; renderOverlay(); return; }
  const r = el.getBoundingClientRect();
  S.menu = kind === "chat" ? { kind, left: r.left, top: r.bottom + 6 } : { kind, left: r.left, bottom: innerHeight - r.top + 8 };
  renderOverlay();
}

function setView(v: View) {
  S.view = v; S.menu = null; S.renamingId = "";
  render();
  if (v === "cluster") { void checkWorkers(); schedulePreview(); void mesh.refreshMesh().then(() => { mesh.schedulePlan(); mesh.paint(true); }); }
}

document.addEventListener("click", (ev) => {
  const target = ev.target as HTMLElement;
  if (S.menu && !target.closest(".menu") && !target.closest("[data-menu]")) { S.menu = null; renderOverlay(); }
  const t = target.closest<HTMLElement>("[data-act],[data-view],[data-chat],[data-del],[data-menu]");
  if (!t) return;
  if (t.classList.contains("modal-back") && target !== t) return; // only the backdrop itself closes
  const d = t.dataset;
  if (d.menu) { openMenu(d.menu as Menu["kind"], t); return; }
  const inMenu = !!t.closest(".menu");
  if (d.view) { setView(d.view as View); return; }
  if (d.chat) { S.activeId = d.chat; S.view = "chat"; S.menu = null; S.ctxTokens = 0; render(); return; }
  if (d.del) { deleteChat(d.del); return; }
  if (d.act?.startsWith("mesh-")) { mesh.meshAct(d.act, d); return; }
  const chat = active();
  switch (d.act) {
    case "new-chat": newChat(); break;
    case "toggle-side": S.side = !S.side; savePrefs(); render(); break;
    case "toggle-right": S.right = !S.right; savePrefs(); render(); break;
    case "search-toggle": S.searchOpen = !S.searchOpen; if (!S.searchOpen) S.query = ""; render(); break;
    case "open-settings": S.modal = S.modal ?? "general"; S.menu = null; renderOverlay(); break;
    case "open-settings-general": S.modal = "general"; S.menu = null; renderOverlay(); break;
    case "close-modal": S.modal = null; renderOverlay(); break;
    case "modal-tab": S.modal = d.tab as Tab; renderOverlay(); break;
    case "send": void send(); break;
    case "stop": void invoke("chat_cancel"); break;
    case "sug": void send(d.text); break;
    case "regen": regenerate(); break;
    case "copy-msg": { const m = chat?.messages[Number(d.i)]; if (m) void navigator.clipboard?.writeText(m.content).then(() => toast("Copied")); break; }
    case "copy-code": { const code = t.closest(".codeblock")?.querySelector("code")?.textContent ?? ""; void navigator.clipboard?.writeText(code).then(() => toast("Code copied")); break; }
    case "rename-chat": S.renamingId = S.activeId; S.menu = null; S.side = true; render(); break;
    case "copy-chat": if (chat) void navigator.clipboard?.writeText(chatMarkdown(chat)).then(() => toast("Chat copied as Markdown")); break;
    case "del-chat": if (chat) deleteChat(chat.id); S.menu = null; break;
    case "install-engine": void installEngine(); break;
    case "chat-cluster": S.useCluster = !S.useCluster; S.menu = null; render(); toast(S.useCluster ? "Chats now go to the whole cluster" : "Chats use the model on this PC"); break;
    case "load-model": if (d.file) void loadModel(d.file); break;
    case "unload": void unloadModel(); S.menu = null; break;
    case "dl": { const c = S.catalog.find((x) => x.id === d.id); if (c) void downloadModel(c.id, c.name, c.url, c.file, c.projUrl && c.projFile ? { url: c.projUrl, file: c.projFile } : null); break; }
    case "attach-image": S.menu = null; renderOverlay(); document.querySelector<HTMLInputElement>("#file-input")?.click(); break;
    case "rm-attach": S.attach.splice(Number(d.i), 1); render(); break;
    case "dl-cancel": void invoke("model_cancel", { id: d.id }); break;
    case "model-delete": void invoke("model_delete", { file: d.file }).then(refreshInstalled).then(render).catch((e) => toast(String(e))); break;
    case "filter": S.filter = d.f as typeof S.filter; render(); break;
    case "open-folder": void invoke("open_models_folder"); break;
    case "custom-dl": {
      const url = $<HTMLInputElement>("#custom-url").value.trim();
      const file = decodeURIComponent(url.split("?")[0].split("/").pop() ?? "");
      if (!url.startsWith("https://") || !file.toLowerCase().endsWith(".gguf")) { $("#custom-status").textContent = "Use an https:// link that ends in .gguf"; break; }
      const projUrl = ($<HTMLInputElement>("#custom-proj")?.value ?? "").trim();
      if (projUrl && !projUrl.startsWith("https://")) { $("#custom-status").textContent = "The vision file link must start with https://"; break; }
      void downloadModel("custom-" + file, file, url, file, projUrl ? { url: projUrl, file: file.replace(/\.gguf$/i, "") + ".mmproj.gguf" } : null);
      break;
    }
    case "clear-chats": S.chats = []; newChat(false); saveChats(); S.modal = null; render(); toast("All chats deleted"); break;
    case "set-think": S.settings.thinking = d.v as Settings["thinking"]; saveSettings(); render(); renderOverlay(); break;
    case "set-theme": S.settings.theme = d.v as Settings["theme"]; saveSettings(); applyTheme(); renderOverlay(); break;
    case "set-scale": S.settings.scale = Number(d.v); saveSettings(); applyTheme(); renderOverlay(); break;
    case "api-genkey": {
      const b = new Uint8Array(18); crypto.getRandomValues(b);
      S.settings.apiKey = "lanlm-" + [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
      saveSettings();
      const el = document.querySelector<HTMLInputElement>("#s-api-key"); if (el) el.value = S.settings.apiKey;
      break;
    }
    case "api-copy-key": void navigator.clipboard?.writeText(S.settings.apiKey).then(() => toast("API key copied")); break;
    case "worker-start": {
      S.clusterMsg = "";
      S.cluster.workerPort = Math.round(Number($<HTMLInputElement>("#w-port").value)) || 50052;
      S.cluster.workerLan = $<HTMLInputElement>("#w-lan").checked;
      saveCluster();
      void invoke<WorkerStatus>("worker_start", { port: S.cluster.workerPort, threads: S.settings.threads, lan: S.cluster.workerLan })
        .then((st) => { S.worker = st; }).catch((e) => { S.clusterMsg = String(e); }).finally(() => { render(); void checkWorkers(); });
      break;
    }
    case "worker-stop": void invoke("worker_stop").then(() => { S.worker = { ...S.worker, running: false, pairing: null, addresses: [] }; render(); void checkWorkers(); }); break;
    case "pair-copy": if (S.worker.pairing) void navigator.clipboard?.writeText(S.worker.pairing).then(() => toast("Pairing code copied")); break;
    case "c-add": addWorker(); break;
    case "c-paste": {
      void navigator.clipboard?.readText().then((txt) => {
        const el = document.querySelector<HTMLInputElement>("#c-endpoint");
        if (el && txt.trim()) { el.value = txt.trim(); el.dispatchEvent(new Event("input", { bubbles: true })); if (txt.includes("@")) addWorker(); }
      }).catch(() => toast("Could not read the clipboard. Paste with Ctrl+V instead."));
      break;
    }
    case "c-check": void checkWorkers(); break;
    case "c-rm": S.cluster.workers.splice(Number(d.i), 1); saveCluster(); render(); schedulePreview(); break;
  }
  if (inMenu && S.menu && d.act !== "rename-chat") { S.menu = null; renderOverlay(); }
});

document.addEventListener("dblclick", (ev) => {
  const t = (ev.target as HTMLElement).closest<HTMLElement>("[data-chat]");
  if (t?.dataset.chat) { S.activeId = t.dataset.chat; S.renamingId = t.dataset.chat; render(); }
});

function finishRename(commit: boolean) {
  const el = document.querySelector<HTMLInputElement>("#rename-input");
  const chat = S.chats.find((c) => c.id === S.renamingId);
  if (el && chat && commit && el.value.trim()) { chat.title = el.value.trim(); saveChats(); }
  S.renamingId = "";
  render();
}
document.addEventListener("focusout", (ev) => { if ((ev.target as HTMLElement).id === "rename-input" && S.renamingId) finishRename(true); });

document.addEventListener("keydown", (ev) => {
  const t = ev.target as HTMLElement;
  if (t.id === "rename-input") { if (ev.key === "Enter") finishRename(true); else if (ev.key === "Escape") finishRename(false); return; }
  if (t.id === "input" && ev.key === "Enter" && !ev.shiftKey && !ev.isComposing) { ev.preventDefault(); void send(); return; }
  if (t.id === "c-endpoint" && ev.key === "Enter") { ev.preventDefault(); addWorker(); return; }
  if (ev.key === "Escape") {
    if (S.modal) { S.modal = null; renderOverlay(); } else if (S.menu) { S.menu = null; renderOverlay(); } else if (S.searchOpen) { S.searchOpen = false; S.query = ""; render(); }
    return;
  }
  if (!ev.ctrlKey || ev.altKey) return;
  const k = ev.key.toLowerCase();
  if (k === "n") { ev.preventDefault(); newChat(); }
  else if (k === "k") { ev.preventDefault(); S.searchOpen = true; S.side = true; render(); }
  else if (k === "b") { ev.preventDefault(); S.side = !S.side; savePrefs(); render(); }
  else if (k === ".") { ev.preventDefault(); S.right = !S.right; savePrefs(); render(); }
  else if (k === ",") { ev.preventDefault(); S.modal = "general"; renderOverlay(); }
});

document.addEventListener("input", (ev) => {
  const t = ev.target as HTMLInputElement;
  if (mesh.meshInput(t, "input")) return;
  const s = S.settings;
  if (t.dataset.wtoggle !== undefined) {
    const w = S.cluster.workers[Number(t.dataset.wtoggle)];
    if (w) { w.off = !t.checked; saveCluster(); render(); schedulePreview(); }
    return;
  }
  switch (t.id) {
    case "c-endpoint": { const wrap = document.querySelector<HTMLElement>("#c-ram-wrap"); if (wrap) wrap.style.visibility = t.value.includes("@") ? "hidden" : ""; return; }
    case "input": t.style.height = "auto"; t.style.height = Math.min(t.scrollHeight, 190) + "px"; return;
    case "search-input": S.query = t.value; { const pos = t.selectionStart; render(); const el = document.querySelector<HTMLInputElement>("#search-input"); el?.setSelectionRange(pos, pos); } return;
    case "s-ctx": s.ctx = Math.max(512, Math.round(Number(t.value)) || s.ctx); break;
    case "s-threads": s.threads = Math.max(1, Math.round(Number(t.value)) || s.threads); break;
    case "s-parallel": s.parallel = Math.max(1, Math.round(Number(t.value)) || s.parallel); break;
    case "s-temp": s.temperature = Number.isFinite(parseFloat(t.value)) ? parseFloat(t.value) : s.temperature; break;
    case "s-max": s.maxTokens = Math.max(16, Math.round(Number(t.value)) || s.maxTokens); break;
    case "s-system": s.system = t.value; break;
    case "s-api-enabled": s.apiEnabled = t.checked; break;
    case "s-api-network": s.apiNetwork = t.checked; break;
    case "s-api-key": s.apiKey = t.value.trim(); break;
    case "s-api-port": s.apiPort = Math.min(65535, Math.max(1024, Math.round(Number(t.value)) || 8080)); break;
    case "c-enabled": S.cluster.enabled = t.checked; saveCluster(); render(); schedulePreview(); return;
    case "c-force": S.cluster.forceShare = t.checked; saveCluster(); schedulePreview(); return;
    case "w-lan": S.cluster.workerLan = t.checked; saveCluster(); return;
    default: return;
  }
  saveSettings();
});

// Images: pick with the + menu, paste into the message box, or drop onto it.
document.addEventListener("paste", (ev) => {
  if ((ev.target as HTMLElement).id !== "input") return;
  const files = [...(ev.clipboardData?.files ?? [])].filter((f) => f.type.startsWith("image/"));
  if (files.length) { ev.preventDefault(); void addImages(files); }
});
document.addEventListener("dragover", (ev) => { if ((ev.target as HTMLElement).closest?.("#cbox")) ev.preventDefault(); });
document.addEventListener("drop", (ev) => {
  if (!(ev.target as HTMLElement).closest?.("#cbox")) return;
  ev.preventDefault();
  void addImages([...(ev.dataTransfer?.files ?? [])]);
});

document.addEventListener("change", (ev) => {
  const t = ev.target as HTMLSelectElement;
  if (mesh.meshInput(t, "change")) return;
  if (t.id === "file-input") {
    const input = t as unknown as HTMLInputElement;
    void addImages([...(input.files ?? [])]).then(() => { input.value = ""; });
    return;
  }
  if (t.id === "pv-model") { S.previewModel = t.value; schedulePreview(); }
  if (t.dataset.ramI !== undefined) {
    const w = S.cluster.workers[Number(t.dataset.ramI)];
    const gbv = Number(t.value);
    if (w && gbv > 0) { w.ramMb = Math.round(gbv * 1024); saveCluster(); render(); schedulePreview(); }
  }
});

boot().catch((e) => { $("#app").innerHTML = `<pre class="error" style="padding:20px">Failed to start: ${esc(String(e))}</pre>`; });
