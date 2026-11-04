# Contributing to LanLM

Thank you for helping make local AI easier to use! LanLM is a small, friendly project, and contributions of every size are welcome: bug reports, documentation fixes, new catalog models, tests, and features.

By taking part you agree to follow our [Code of Conduct](CODE_OF_CONDUCT.md). To report a **security** problem, please do **not** open a public issue: follow [SECURITY.md](SECURITY.md).

## Ways to contribute

| I want to… | Do this |
| --- | --- |
| Report a bug | [Open a bug report](https://github.com/sakthiveltofficial/lanlm/issues/new/choose). Include your Windows version, RAM, the model you used and the **Engine log** (right panel → _Engine log_). |
| Suggest a feature | [Open a feature request](https://github.com/sakthiveltofficial/lanlm/issues/new/choose) and describe the problem you want solved. |
| Fix something small | Fork, fix, open a pull request. Typos and doc fixes need no issue. |
| Build something bigger | Open an issue first so we can agree on the approach before you invest time. |
| Add a model to the catalog | See [Adding a model](#adding-a-model-to-the-catalog). |
| Help test | Try the app on your hardware, especially vision models and clusters on a real network, and report what you find. |

Look for issues labelled **good first issue** or **help wanted**.

## Getting set up

You need Windows 10/11, **Node.js 20+**, **Rust (stable)**, the **Visual Studio C++ Build Tools** and **WebView2**.

```bash
git clone https://github.com/sakthiveltofficial/lanlm.git
cd lanlm   # your clone's folder name
npm install
npm run tauri dev
```

The first Rust build takes several minutes. Later builds are fast.

### Project map

| Path | What lives there |
| --- | --- |
| `src/main.ts` | The whole interface: state, templates and event handlers (plain TypeScript, no framework) |
| `src/lib/` | Pure, unit-tested helpers: Markdown rendering, formatting, pairing-code parsing, icons |
| `src-tauri/src/` | Rust backend. `engine.rs` installs llama.cpp, `models.rs` is the catalog, `server.rs` runs `llama-server`, `cluster.rs` plans and runs the cluster, `chat.rs` streams replies, `dl.rs` downloads with resume |
| `docs/ARCHITECTURE.md` | How the pieces fit together. Read this before a larger change. |

## Making a change

1. **Fork** the repository and create a branch from `main`: `git checkout -b fix/short-description`.
2. **Make your change**, keeping it focused. One pull request should do one thing.
3. **Add or update tests** for any behaviour you change (see below).
4. **Run the checks** locally: `npm run verify`.
5. **Commit** with a clear message. We like [Conventional Commits](https://www.conventionalcommits.org): `fix: …`, `feat: …`, `docs: …`, `test: …`, `refactor: …`, `chore: …`.
6. **Open a pull request** and fill in the template. Link the issue it closes (`Closes #123`).
7. **Respond to review.** We aim to reply within a few days. Push follow-up commits to the same branch.

## Running the checks

```bash
npm run verify        # everything below, in order, with a summary
npm run verify:fast   # frontend only (no Rust build), good while iterating
```

`npm run verify` runs the same steps as CI, so if it passes locally your pull request should pass too:

| Step | Command | What it catches |
| --- | --- | --- |
| Versions match | `npm run check:versions` | `package.json`, `Cargo.toml` and `tauri.conf.json` disagreeing |
| TypeScript types | `npm run typecheck` | Type errors |
| ESLint | `npm run lint` (`lint:fix` repairs many) | Bugs and unused code |
| Prettier | `npm run format:check` (`format` repairs) | Inconsistent formatting in `src/lib` and `scripts` |
| Frontend unit tests | `npm test` (`test:watch`, `test:coverage`) | Logic regressions in `src/lib` and UI consistency |
| Frontend build | `npm run build` | A broken production build |
| Rust formatting | `npm run rust:fmt` (run `cargo fmt` to fix) | Formatting |
| Rust lint | `npm run rust:clippy` | Common Rust mistakes (warnings are errors) |
| Rust tests | `npm run rust:test` | Planner, downloads, catalog, parsing |

On every pull request GitHub Actions also builds the Windows app, runs CodeQL and audits dependencies. A **CI success** check must be green before merging.

### Writing tests

- **Put logic in testable places.** Pure functions go in `src/lib/` (TypeScript) or a Rust module with a `tests.rs` file next to it, and get unit tests. Keep `main.ts` for wiring.
- **Rust tests** live in `src-tauri/src/<module>/tests.rs` and are included with `#[cfg(test)] mod tests;`. Do not write multi-gigabyte files in tests: use the number-based helpers (see `cluster/tests.rs`). The downloader tests run a small local HTTP server, so tests never need the internet.
- **UI consistency tests** (`src/app.consistency.test.ts`) fail if a button has no click handler, an icon name does not exist, or model output could be inserted into the page without escaping. When you add a button with `data-act="my-action"`, add a matching `case "my-action":` in the click handler.
- **A bug fix needs a test** that fails without the fix.
- Tests must not depend on the network, a GPU, a downloaded model, or the order they run in.

## Code style

- **TypeScript:** strict mode, no framework. Escape anything user- or model-supplied with `esc()` or `md()` before putting it into HTML. Run `npm run format` and `npm run lint:fix`.
- **Rust:** `cargo fmt` and no clippy warnings. Return `Result<_, String>` from commands with a message a user could act on. Keep OS-specific code behind `#[cfg(windows)]`.
- **UI text:** plain, friendly language. Say what happened and what to do next. Avoid jargon (write "layers on this PC", not "tensor split").
- **Dependencies:** think twice before adding one. Prefer the standard library. New dependencies need a reason in the pull request.
- **Security:** validate file names and URLs that come from the UI (see `safe_name` in `models.rs`), never run user-supplied strings through a shell, and keep network listeners opt-in.

## Adding a model to the catalog

The catalog is the `CATALOG` list in `src-tauri/src/models.rs`. Add an `Entry` with:

- a unique `id`, a friendly `name` and a one-line `desc`
- the Hugging Face `repo` and exact GGUF `file` name (it must be a direct download, usually `Q4_K_M` or `Q4_0`)
- `size_mb`, the file size in megabytes
- for vision models, `proj_file` (the `mmproj-….gguf` file) and `proj_mb`; otherwise `""` and `0`

Then:

1. Confirm the download URL works: `https://huggingface.co/<repo>/resolve/main/<file>`.
2. Confirm the model **license allows redistribution links** and is not gated behind a login.
3. Run `npm run rust:test`. The catalog tests check ids, URLs, file-name safety and projector rules.
4. Update the model table in `README.md`.

## Pull request checklist

- [ ] `npm run verify` passes
- [ ] Tests added or updated, and a bug fix includes a regression test
- [ ] Docs updated (`README.md`, `docs/`, `CHANGELOG.md` under **Unreleased**)
- [ ] No unrelated changes, secrets, large binaries or personal data
- [ ] Screenshots attached for UI changes (light and dark if relevant)

## Releases (maintainers)

Versions follow [Semantic Versioning](https://semver.org). See [docs/PUBLISHING.md](docs/PUBLISHING.md) for the release steps. Pushing a `vX.Y.Z` tag runs the tests, builds the Windows installer and portable `.exe`, and publishes a GitHub release with checksums.

## License of contributions

LanLM is [MIT licensed](LICENSE). By submitting a contribution you agree that it is licensed under the same terms, and that you have the right to submit it.

## Questions

Open a [discussion](https://github.com/sakthiveltofficial/lanlm/discussions) or an issue. There are no silly questions.
