# Changelog

All notable changes to LanLM are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [Semantic Versioning](https://semver.org).

## [Unreleased]

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

[Unreleased]: https://github.com/sakthiveltofficial/lanlm/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/sakthiveltofficial/lanlm/releases/tag/v0.1.1
[0.1.0]: https://github.com/sakthiveltofficial/lanlm/releases/tag/v0.1.0
