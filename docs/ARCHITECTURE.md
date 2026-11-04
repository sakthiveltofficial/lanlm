# Architecture

LanLM is a thin, friendly layer over [llama.cpp](https://github.com/ggml-org/llama.cpp). It does **not** link llama.cpp: it downloads the official release and runs its programs (`llama-server.exe`, `ggml-rpc-server.exe`) as separate processes. That keeps LanLM small, lets llama.cpp be updated without rebuilding the app, and means an engine crash cannot take the interface down.

```
┌───────────────────────────── LanLM (Tauri 2) ─────────────────────────────┐
│  Interface (TypeScript, WebView2)           Backend (Rust)                  │
│  ┌───────────────────────────┐   invoke    ┌────────────────────────────┐  │
│  │ src/main.ts   state+views │ ──────────▶ │ engine.rs   install llama  │  │
│  │ src/lib/      pure logic  │ ◀────────── │ models.rs   catalog, files │  │
│  └───────────────────────────┘   events    │ dl.rs       resumable HTTP │  │
│                                            │ server.rs   llama-server   │  │
│                                            │ cluster.rs  plan + workers │  │
│                                            │ chat.rs     stream replies │  │
│                                            │ hw.rs       CPU / RAM      │  │
│                                            └─────────────┬──────────────┘  │
└──────────────────────────────────────────────────────────┼─────────────────┘
                                                           │ spawns
                    ┌──────────────────────────────────────┴──────────────┐
                    │ llama-server.exe  (OpenAI-style HTTP, 127.0.0.1)    │
                    │   └─ --rpc host:port ─▶ ggml-rpc-server.exe (worker)│
                    └─────────────────────────────────────────────────────┘
```

## A chat message, step by step

1. The interface calls the Rust command `chat_stream` with the conversation.
2. Rust POSTs to the local `llama-server` (`/v1/chat/completions`, `stream: true`) and reads the server-sent events.
3. Each token is sent back to the interface through a Tauri `Channel` as a `delta`, `reasoning` or `stats` event.
4. The interface appends text and re-renders just the last message (`paintLastMessage`). Output always goes through `md()`, which escapes HTML.

Going through Rust instead of calling `llama-server` from the webview avoids CORS problems and lets the API key stay on the Rust side.

## Starting a model (`server.rs`)

`server_start` picks the command line for `llama-server`:

- `-m <model>`, `-c <context>`, `-t <threads>`, `-np <slots>`
- `--mmproj <file>` when a vision projector sits next to the model (`<model>.mmproj.gguf`)
- `--api-key` and a fixed port/host when the API is shared, otherwise a random loopback port
- cluster arguments from `cluster.rs` when workers are used

It then polls `/health` until the model has loaded, and keeps the child process so it can be stopped. The process is also killed when the app exits.

## The cluster (`cluster.rs`)

llama.cpp has a built-in **RPC backend**. A worker runs `ggml-rpc-server`; the master passes `--rpc host:port` and llama.cpp treats each worker as a remote device. llama.cpp offloads the **last** `-ngl` layers to those devices, split by `-ts`.

The planner (`split_sizes`, a pure function tested without model files) decides the numbers:

1. Read the model's layer count from its GGUF header (`block_count`, which skips large arrays such as the tokenizer).
2. Estimate memory need: `file size × 1.1` (weights plus KV cache and buffers).
3. The master offers about 70% of its free RAM; each worker lends 75% of the free RAM it reported.
4. If the model fits on the master, nothing is offloaded (faster, no network hops) unless the user forces sharing.
5. Otherwise the master keeps as many layers as fit and the rest are spread over workers by memory (`spread`, largest-remainder rounding).
6. `-ngl = layers + 1 − local layers`. The `+1` is the output layer, which is pinned back to the master with `-ot "^(output|token_embd)\.weight$=CPU"`, because its result (one score per vocabulary word) would otherwise cross the network for every token.

The same planner feeds the **Preview** (`cluster_preview`), so what the user sees is exactly what a load would do.

Limits worth knowing: an RPC worker serves one client at a time, has no authentication or encryption, and every token makes a network round trip.

## Downloads (`dl.rs`)

`download()` streams a URL into `<file>.part` and renames it on success. It resumes with an HTTP `Range` request, treats a `416` as "already complete", and keeps the `.part` file when cancelled. It has no knowledge of Tauri, so it is tested against a tiny local HTTP server.

## The interface (`src/`)

The interface is plain TypeScript that renders HTML strings and uses one delegated click handler keyed by `data-act` attributes. This keeps the app dependency-light, but it means conventions matter:

- Every `data-act="x"` in a template needs a `case "x":` in the click handler. `app.consistency.test.ts` enforces this.
- Everything from a user or model is escaped (`esc`, `md`).
- Logic that can be tested without a browser lives in `src/lib/`.

## Testing strategy

| Layer | Tool | Notes |
| --- | --- | --- |
| Pure TypeScript | Vitest | `src/lib/*.test.ts` |
| UI wiring | Vitest static checks | Missing handlers, bad icon names, unescaped output |
| Rust units | `cargo test` | Planner, GGUF parsing, catalog, downloads, asset picking |
| Style and bugs | ESLint, Prettier, rustfmt, clippy | Warnings are errors |
| Whole app | CI `app-build` job | Builds the Windows executable on every PR |

There are no end-to-end tests that drive the real window or a real model yet. Contributions in that area are welcome.
