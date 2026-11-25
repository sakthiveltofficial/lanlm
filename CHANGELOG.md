# Changelog

All notable changes to LanLM are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [Semantic Versioning](https://semver.org).

## [Unreleased]

### Added

- **New look**: a light, soft glass window over a peach gradient (and a matching dark theme), a left navigation, a centered welcome with suggestion cards, a rounded composer with Attach, model and Options buttons, and a chat-history panel on the right with search, previews and where each chat ran.
- **One-click start**: the welcome screen suggests a model that fits this PC and installs the engine, downloads and loads it in one go.
- **Run on all PCs** from the Models page: one click starts a model across the cluster (files are copied PC to PC).
- **One model picker** under the message box lists models running on the cluster and on this PC. Chats switch to the cluster on their own when a model is ready there and nothing runs on this PC.
- **Join requests everywhere**: a PC asking to join shows as a banner on every page (and a badge on Devices), so the two-minute window is not missed. The joining PC shows the code to compare in the same place.
- **Add a PC** button always visible in the sidebar; on a PC that is not in a cluster yet it creates one first.
- A PC that joins a cluster installs its engine on its own.
- **Telegram bot** (new `telegram` module in `lanlm-mesh`): chat with your PCs from your phone. Long polling, an allow-list with approval in the app, `/new` and `/status`, and the same in `lanlm-node` with `--telegram-token` and `--telegram-allow`.

### Changed

- The Cluster page is now **Devices**; its Shared API tab moved to its own **Shared API** page, which also holds this PC's API settings when it is not in a cluster. The Run tab hides mode, context and slot settings under **Advanced options**.
- Hardware, context and task details moved out of the right panel: the context use shows in the composer, background tasks in a header pill, the engine log under Settings → Model & engine.

## [0.2.0] - 2026-11-06

The peer-to-peer cluster release.

### Added

- **Peer-to-peer cluster** (new `lanlm-mesh` crate). PCs find each other on the local network, join only after a person presses Allow (both screens show the same six-digit code), and talk over encrypted, mutually authenticated links (Noise `XXpsk3`). There is no coordinator: every PC publishes a record of itself (hardware, GPUs, engine, speed, model files, running layers, load) and the records are merged between all PCs.
- **Planner** that decides where a model runs from memory, GPU or CPU, measured speed and network delay: a full copy on every PC that can hold it, and a split across PCs only for what fits nowhere. It leaves out a PC whose network hop costs more than it saves.
- **Self-healing deployments.** A deployment is kept running: a crashed server is restarted, a vanished PC is replaced, and a PC that joins is used.
- **Peer-to-peer model store.** Models are copied between PCs in hash-checked 4 MiB pieces, from several PCs at once, resumable, with the internet used only for pieces nobody on the network has.
- **Shared gateway**: one OpenAI-compatible address for the whole cluster with least-loaded routing, a waiting line, conversation stickiness (prompt-cache reuse), failover and API keys.
- **GPU engine install**: picks the llama.cpp build for the PC's graphics card (CUDA 12/13, Vulkan, CPU) and checks that it sees the GPU, falling back to the next build if not.
- **`lanlm-node`**, a headless daemon with a local control API and command line, for Linux and Windows servers.
- **Cluster screen** in the app: devices, models, run, shared API and activity, plus chat with the whole cluster.

### Changed

- Cluster traffic never uses llama.cpp's unauthenticated RPC port directly: workers listen on this machine only and are reached through the encrypted tunnel.
- The download code moved into the shared crate; the older "add a PC by address" flow is kept under **Cluster → More**.
- Rust 1.95 or newer is required (as before for the existing dependencies).

### Fixed

- A download that learns its size from a HEAD request no longer treats an empty body as a 0-byte file.

## [0.1.1] - 2026-11-05

First published release. Version 0.1.0 was tagged but never published, because its release build failed (see _Fixed_).

### Added

- The release workflow publishes the Windows installer, a portable single-file `.exe` and `SHA256SUMS.txt` to GitHub Releases when a version tag is pushed.

### Changed

- Dependency updates: Vite 8, `zip` 9, `sysinfo` 0.40, and newer GitHub Actions (`checkout`, `setup-node`, `upload-artifact` 7; `tauri-action` 1).
- Engine unpacking is covered by tests, including protection against archives that try to write outside the target folder.

### Fixed

- The release build failed with "Invalid TOML document" because a byte-order mark had been added to the start of `Cargo.toml`. It is removed from every file, and a test now guards against it.

## [0.1.0] - 2026-11-05

Not published (the release build failed; see 0.1.1).

### Added

- Desktop app for Windows built with Tauri 2, Rust and TypeScript.
- One-click installer for the official llama.cpp Windows CPU build.
- Model catalog with "fits this PC" badges, resumable downloads, and support for any GGUF link or file.
- Chat with streaming replies, Markdown and copyable code blocks, collapsible thinking, regenerate, message queue, chat search and rename, light and dark themes, and keyboard shortcuts.
- Context meter, hardware-aware default settings, and an engine log panel.
- Cluster mode: pair PCs with a code, plan layer placement, and preview the split with a master/worker graph, layer map and memory bars.
- Local OpenAI-compatible API server with API key support and optional network access.
- Parallel request slots served by llama.cpp batching.
- Vision model support (experimental): vision catalog entries, automatic projector pairing, and image attach, paste and drop.
- Unit tests for the Rust backend and the TypeScript helpers, linting, formatting, and GitHub Actions CI, release and CodeQL workflows.

[Unreleased]: https://github.com/sakthiveltofficial/lanlm/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/sakthiveltofficial/lanlm/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/sakthiveltofficial/lanlm/releases/tag/v0.1.1
[0.1.0]: https://github.com/sakthiveltofficial/lanlm/releases/tag/v0.1.0
