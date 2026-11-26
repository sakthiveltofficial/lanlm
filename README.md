<div align="center">

<img src="docs/images/banner.png" alt="LanLM: a true peer-to-peer AI cluster for your own PCs. No server, no cloud, no account." width="860">

# LanLM

## A true, native peer-to-peer AI cluster for the PCs you already own.

**No server. No broker. No cloud. No account.**
Every PC is an equal peer. They find each other, share what they have, and run models together, from a 1B chat model on a laptop to a 70B model that no single machine could hold.

[![CI](https://github.com/sakthiveltofficial/lanlm/actions/workflows/ci.yml/badge.svg)](https://github.com/sakthiveltofficial/lanlm/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/sakthiveltofficial/lanlm?include_prereleases&label=release)](https://github.com/sakthiveltofficial/lanlm/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platform: Windows](https://img.shields.io/badge/app-Windows%2010%2F11-0078D4)
![Headless: Linux and Windows](https://img.shields.io/badge/headless%20node-Linux%20%7C%20Windows-444)
![Built with Tauri 2](https://img.shields.io/badge/built%20with-Tauri%202%20%2B%20Rust-24C8DB)
[![PRs welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](CONTRIBUTING.md)

[Why peer-to-peer](#why-this-is-different) · [How it works](#how-the-cluster-works) · [Quick start](#quick-start) · [Shared API](#one-openai-compatible-address-for-the-whole-cluster) · [Run it headless](#run-it-on-servers-no-screen) · [Honest limits](#honest-limits) · [Build](#build-from-source)

</div>

---

## Why this is different

Most "distributed AI" setups are really a **client and a server**: one machine you must set up first, a coordinator that everything depends on, or a cloud account in the middle. LanLM has none of that.

|  | Typical cluster tools | **LanLM** |
| --- | --- | --- |
| Who is in charge | One coordinator or head node | **Nobody. Every PC is a peer.** |
| Finding each other | Type IP addresses and ports | **Automatic** on your network (mDNS), one click to join |
| Letting a PC in | Open port, hope for the best | **You approve it**: both screens show the same 6-digit code |
| Traffic between PCs | Plain TCP | **End-to-end encrypted** and mutually authenticated (Noise protocol) |
| Models | Copy files by hand to every machine | **PCs copy models from each other**, hash-checked, resumable |
| Mixed hardware | CPU-only, or one GPU type | **GPU and CPU together**: CUDA, Vulkan, or CPU, chosen per PC |
| A PC dies | Everything stops | **The cluster heals itself** and puts the model back on the PCs that are left |
| A PC joins | Reconfigure | **Used automatically**, with no restart |
| Talking to it | A different API per tool | **One OpenAI-compatible address** that load-balances across everything |
| Where your data goes | Often a vendor | **Nowhere.** Prompts and models never leave your network. |

> **In one sentence:** open LanLM on a few PCs, press **Invite a PC**, and you have one private AI that is as big and as fast as all of them put together.

<div align="center">

[![Watch the LanLM demo video (1 min 47 s)](docs/demo/poster.jpg)](docs/demo/lanlm-demo.mp4)

**[▶ Watch the demo](docs/demo/lanlm-demo.mp4)**

</div>

## What you get

- 🌐 **Peer-to-peer by design.** There is no master you must keep alive. Any PC can serve the API, start a model, or invite a new member. The cluster state is shared between all PCs, so every screen shows the same truth.
- 🔎 **Transparent.** Every PC publishes what it has and what it is doing: its CPU, memory, GPUs, installed engine, measured speed, which model files it holds, which **layers** of which model it is running, whether it is busy, and how fast it answers. You see it all in one place.
- ⚡ **Real throughput, not just a bigger model.** Each PC that can run a model on its own does, and the shared API spreads requests across all of them. Every copy batches its requests continuously, and a request that arrives when everything is busy waits in line instead of failing.
- 🧩 **Models too big for one PC.** A model that fits no single machine is split layer by layer across several PCs, weighted by how fast and how roomy each one is. A PC whose network hop would cost more than it saves is left out.
- 🎮 **Uses your GPUs.** LanLM looks at each PC's graphics hardware, installs the llama.cpp build that fits (NVIDIA CUDA, or Vulkan for AMD and Intel), checks that it really sees the GPU, and falls back to CPU if not. Mixed clusters work: a CUDA desktop, a Vulkan laptop, and a CPU mini-PC in one cluster.
- 🔒 **Private and secure by default.** See [Security](#security-model).
- 🧠 **A full desktop app too.** Streaming chat, Markdown, vision models, model catalog, light and dark themes, and a local OpenAI-compatible API, all offline.

## How the cluster works

```
        ┌──────────────┐        encrypted mesh (Noise)       ┌──────────────┐
        │ Desktop      │◀───────────────────────────────────▶│ Laptop       │
        │ RTX 5060 Ti  │   shared state: who has what,       │ CPU          │
        │ whole model  │   what runs where, how busy         │ whole model  │
        └──────┬───────┘                                     └──────┬───────┘
               │            ┌──────────────┐                        │
               └───────────▶│ Mini-PC      │◀───────────────────────┘
                            │ CPU          │
                            │ spare: holds │   Any PC can run the shared API.
                            │ layers 20-31 │   Requests go to the PC with the
                            └──────────────┘   most room, from anywhere.
```

1. **Find.** PCs announce themselves on your network. A PC joins only after you press **Allow** and see that both screens show the same code, so nobody can slip in. Where multicast is blocked you can connect by address.
2. **Share.** Every PC keeps one small record about itself and tells all the others. Records are merged, so there is no database and no single point of failure. A PC that goes silent disappears from the table within seconds.
3. **Plan.** Ask for a model and the cluster works out where it should live: a **full copy on every PC that can hold it** (fastest, no network in the loop), and a **split across PCs** only for what does not fit anywhere. The plan accounts for memory, GPU versus CPU, measured speed and network delay, and you can preview it before you start.
4. **Move.** Missing model files are copied PC to PC, in small pieces from several PCs at once, each checked against a hash. PCs that are downloading the same model swap pieces with each other, so the internet is used roughly **once for the whole cluster**.
5. **Run.** Each PC starts [llama.cpp](https://github.com/ggml-org/llama.cpp), using its GPU if it has one. For a split model, the PC that runs the chat reaches the others through **encrypted tunnels**, so the unauthenticated llama.cpp worker port is never exposed to your network.
6. **Heal.** If a PC disappears or a model crashes, the cluster cleans up and places the model again on whatever is left. When a new PC joins, it is put to work.

### What each PC holds

LanLM keeps three things apart, so nothing is copied that does not need to be:

| | What it is | Where it lives |
| --- | --- | --- |
| **Memory** | The layers a PC is actually computing | **Only the PC's share.** A PC holding layers 20–31 holds only those in RAM or VRAM. |
| **Weights sent to workers** | The tensors a worker was given | A **persistent cache on that worker**, so loading again does not resend gigabytes |
| **Model files** | The full `.gguf` | Only on PCs that run a whole copy, or that run the chat for a split model, and copied from neighbours instead of the internet when possible |

## Quick start

1. **Download** from the [Releases page](https://github.com/sakthiveltofficial/lanlm/releases): the installer (`LanLM_<version>_x64-setup.exe`) or the single-file portable `.exe`. Both are built in public by GitHub Actions, and `SHA256SUMS.txt` lets you verify them.
   _Windows SmartScreen may warn about an unrecognised publisher because the files are not code-signed yet._
2. **Install the engine.** On first launch click **Install engine**. LanLM picks the build for your graphics card automatically.
3. **Start a cluster** on the first PC: **Cluster → Create**.
4. **Add the others.** On each, open **Cluster**; the first PC will appear under *nearby*. Press **Ask to join**, then press **Allow** on the first PC after checking that the six digits match.
5. **Add a model** on the **Models** tab, then press **Start** on the **Run** tab. The preview shows exactly what will happen before it does.
6. **Chat**, or point your tools at the [shared API](#one-openai-compatible-address-for-the-whole-cluster).

Allow TCP port **50500** (and UDP 5353 for discovery) for your private network in the Windows firewall.

### System requirements

|  | Minimum | Comfortable |
| --- | --- | --- |
| OS | Windows 10 or 11, 64-bit (needs WebView2) for the app; Linux or Windows for headless nodes | Windows 11 |
| RAM | 8 GB (1B–3B models) | 16 GB or more |
| GPU | Optional | NVIDIA (CUDA), or AMD/Intel (Vulkan), recent driver |
| Network | Any LAN | **Wired Ethernet** for split models |

## One OpenAI-compatible address for the whole cluster

Turn on **Cluster → Shared API**. Every app, SDK and coding tool that speaks the OpenAI API can use the whole cluster through one address:

```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H "Authorization: Bearer YOUR_KEY" \
  -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","content":"Say hello in five words."}]}'
```

What the gateway does that a plain server does not:

- **Least-loaded routing.** Each request goes to the PC with the most free capacity, preferring faster PCs.
- **A waiting line, not errors.** When every PC is busy, requests wait and are dispatched the moment a slot frees up (each PC keeps batching what it has).
- **Conversation stickiness.** One conversation stays on one PC, so that PC's prompt cache is reused and long chats stay fast.
- **Failover.** If a PC fails before it answers, the request is retried on another one.
- **Several models at once.** Different PCs can run different models; the `model` field picks one.
- **Keys and network control.** Local-only by default. Sharing beyond this PC requires an API key. Streaming (`stream: true`) works.
- Run it on **any** PC. Several gateways can run at the same time.

## Run it on servers, no screen

`lanlm-node` is the same cluster engine as a headless program for Linux and Windows boxes (home lab, office, rack), with a scriptable local API. A PC running the desktop app and a server running `lanlm-node` are equal peers in the same cluster.

```bash
# First box: install the right engine for its hardware, start a cluster, serve the API
lanlm-node install-engine
lanlm-node run --create-cluster home --gateway 8080 --gateway-key MY_KEY

# Every other box: join and lend itself
lanlm-node run --join lanlm1-XXXXX-XXXXX-…

# From any box
lanlm-node status                    # every PC, its hardware, models, layers, load
lanlm-node preview model.gguf        # where would it go?
lanlm-node deploy model.gguf --mode auto --parallel 4
lanlm-node fetch model.gguf --url https://…   # one download, shared with the whole cluster
lanlm-node invite                    # let a new box ask to join (then: allow ID)
```

## Security model

Peer-to-peer on a LAN needs to be safe on a LAN. This is what LanLM does and does not do.

- **Encrypted and authenticated links.** Every connection between PCs uses the [Noise](https://noiseprotocol.org) `XXpsk3` handshake with a per-PC identity key and the cluster key. A PC without the key cannot complete the handshake, traffic cannot be read or altered, and each PC learns the other's identity from the handshake, not from what it claims.
- **Joining needs a human.** Pairing uses a key-less handshake and shows a six-digit code derived from it on both screens. The code matches only when nobody is in the middle. The cluster key is sent only after you press **Allow**.
- **No exposed workers.** llama.cpp's RPC worker has no password. LanLM binds it to the local machine only and exposes it solely through the encrypted tunnel, so the old "anyone on the network can use this PC" problem does not exist in cluster mode.
- **Verified models.** Copied models are checked against a hash taken from the download source or from the PCs that already have them. A bad copy is deleted, not used.
- **Remote data is untrusted.** Names, file names and messages from other PCs are escaped before they are shown, and checked by a test.
- **What it does not do:** it does not defend against a PC that **already holds the cluster key** (members trust each other: a member can read prompts that reach it and run work on others), and it does not hide prompts from the PCs that compute them. Treat a cluster like a shared household or office, not like the internet. See [SECURITY.md](SECURITY.md).

## Honest limits

LanLM is young. We would rather tell you than have you find out.

- **Splitting a model costs speed.** Every generated token visits every PC in the chain, so a split model on Wi-Fi is slow and on wired gigabit is decent. Splitting is for **fitting** a model, not for making a small one faster. Throughput comes from **copies**, and the planner prefers copies whenever they fit.
- **A split model needs the model file on one PC.** llama.cpp's RPC mode reads every tensor on the PC that runs the chat and sends each worker its layers. Workers keep only their share (and cache it), but one PC in a split group must have the whole file. Removing even that needs changes inside llama.cpp.
- **A split model stops if one of its PCs goes away.** It is detected within seconds, cleaned up and placed again on the remaining PCs; full copies on other PCs keep answering the whole time.
- **Deployments are owned by the PC that started them.** That PC keeps them healthy. If it is switched off, the models keep running, and another PC can adopt the deployment.
- **Verified in tests, not yet on a big real fleet.** The cluster, encryption, planner, model transfer, gateway and self-healing are covered by automated tests that run multi-PC clusters with stand-in llama.cpp programs. The GPU engine selection is tested against the real llama.cpp release layout. Real-model, real-network runs across many machines are what we most need help measuring: please [report numbers](https://github.com/sakthiveltofficial/lanlm/issues/new/choose).
- **Windows desktop app, Windows and Linux headless node.** macOS is not built yet.
- **Not code-signed yet**, so Windows SmartScreen warns.

## Features, in full

- **Models.** Any GGUF model llama.cpp supports. Built-in catalog (Llama, Gemma, Qwen, Phi, LFM, vision models), paste any `https://…/model.gguf`, or drop files in the models folder. Fit badges per PC.
- **Engine.** One-click install of the right llama.cpp build per PC (CPU, Vulkan, CUDA 12/13), checked against the PC's real devices.
- **Chat.** Streaming, Markdown, copyable code, collapsible thinking, regenerate, message queue, chat search and rename, images for vision models, keyboard shortcuts, light and dark. Chat with the model on this PC, or with the **whole cluster**.
- **Cluster screen.** Devices, models, run, shared API, and an activity log: layer maps showing which PC holds which layers, memory and GPU bars, speed measurement per PC, lend and empty switches.
- **Local API.** The model on this PC through `/v1/chat/completions`, with an API key, local or network.
- **Quality.** Unit tests, multi-node integration tests, linting, formatting, CodeQL and dependency audits in CI.

## Build from source

You need **Node.js 20+**, **Rust (stable)**, the **Visual Studio C++ Build Tools** ("Desktop development with C++") and Microsoft **WebView2** (already on Windows 11) for the desktop app. The headless node needs only Rust.

```bash
git clone https://github.com/sakthiveltofficial/lanlm.git
cd lanlm
npm install
npm run tauri dev        # run the app with hot reload
npm run tauri build      # build the Windows installer

# the headless node (Linux or Windows)
cargo build --release --manifest-path src-tauri/Cargo.toml -p lanlm-mesh --bin lanlm-node
```

## Quality: tests, linting and CI

```bash
npm run verify        # versions, types, ESLint, Prettier, unit tests, build, rustfmt, clippy, Rust tests
npm run verify:fast   # frontend checks only (no Rust build)
```

The mesh has its own test suite (`cargo test -p lanlm-mesh`): encryption and pairing, state merging and liveness, the planner, the model store, the gateway, and end-to-end clusters of several nodes on one machine that deploy, split, fail over and heal. See [CONTRIBUTING.md](CONTRIBUTING.md#running-the-checks).

## Project layout

```
src/                       TypeScript interface (no UI framework); pure, tested helpers in src/lib
src-tauri/src/             Desktop app backend: local chat, engine, model server, cluster commands
src-tauri/crates/lanlm-mesh/   The cluster engine (also the headless `lanlm-node`):
    crypto, link, mesh         identity, encrypted links, discovery, shared state
    planner, orchestrator      where models go, and keeping them running
    store                      peer-to-peer model files
    gateway                    the shared OpenAI-compatible address
docs/                      Architecture, publishing, branding
```

## Roadmap

- [x] Peer-to-peer mesh with encrypted, mutually authenticated links
- [x] Automatic discovery and human-approved pairing
- [x] Shared cluster state, transparent per-PC models, layers and load
- [x] GPU-aware engine install (CUDA, Vulkan) with verification
- [x] Speed- and memory-aware planner: full copies plus split models
- [x] Peer-to-peer, hash-verified, resumable model copying
- [x] Self-healing deployments
- [x] Load-balancing gateway with queueing, stickiness and failover
- [x] Headless `lanlm-node`
- [ ] Measured benchmarks on real multi-machine setups, published in the README
- [ ] Relay for PCs on different networks (a mesh that spans sites)
- [ ] Spare copies of split models for instant failover
- [ ] Signed Windows installers and auto-update
- [ ] macOS and Linux desktop builds
- [ ] In-app Hugging Face model search

Have an idea? [Open a feature request](https://github.com/sakthiveltofficial/lanlm/issues/new/choose).

## FAQ

**Is it really peer-to-peer?**
Yes. There is no central server, broker, account or cloud service. PCs discover each other on your network, authenticate each other with a shared key, exchange state directly, and any PC can serve the API or start a model. (The one thing that is per-model rather than shared is *which PC runs the chat for a split model*; the planner chooses it, and it can be any PC.)

**Do I need a GPU?**
No. Without one, LanLM uses the CPU build. With one, it uses it automatically, and a cluster can mix both.

**Will a bigger cluster make my model faster?**
More PCs that can each hold the model means more **requests per second**. Splitting one model across PCs lets you run a **bigger** model, not a faster one.

**Is my data private?**
Prompts and models stay on your PCs and your network. LanLM has no account and no telemetry. It contacts GitHub (engine) and Hugging Face (models) only when you ask it to, and only for what no PC in your cluster already has.

**What if a PC leaves?**
It disappears from the table within seconds. A model that used it is cleaned up and placed again on the others, while other copies keep answering.

**Does it work over the internet or between offices?**
Not yet. The mesh is built for one network. A relay for multi-site clusters is on the roadmap. Do not expose the cluster port to the internet.

**How is this different from running llama.cpp by hand?**
LanLM adds everything around it: discovery, secure links, shared state, planning, model transfer, the gateway, self-healing, and an app on top, so you do not manage binaries, flags, ports or scripts.

**Windows says "unknown publisher". Is that safe?**
The installer is not code-signed yet. You can inspect and build the source yourself, and the release is built in public on GitHub Actions.

**How do I contribute?**
Read [CONTRIBUTING.md](CONTRIBUTING.md). Real-hardware benchmarks, bug reports, tests and documentation are especially welcome.

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) and our [Code of Conduct](CODE_OF_CONDUCT.md) first. To report a vulnerability privately, see [SECURITY.md](SECURITY.md).

## Acknowledgements

LanLM stands on the shoulders of [llama.cpp](https://github.com/ggml-org/llama.cpp) (MIT) by Georgi Gerganov and contributors, [Tauri](https://tauri.app), the [Noise Protocol Framework](https://noiseprotocol.org) and the [`snow`](https://github.com/mcginty/snow) implementation, [Hugging Face](https://huggingface.co) for model hosting, and the authors of the open models in the catalog. The idea of pooling devices for one model, and the care Pooled takes over honest security notes and speed-aware placement, was an inspiration.

## License

[MIT](LICENSE) © 2026 LanLM contributors.

<sub>**Keywords:** peer-to-peer LLM cluster, distributed local LLM, run LLM on multiple PCs, pool GPUs and CPUs, llama.cpp cluster, OpenAI-compatible load balancer, private self-hosted AI, serverless AI cluster, encrypted mesh, Windows, Linux, Tauri, Rust, TypeScript.</sub>
