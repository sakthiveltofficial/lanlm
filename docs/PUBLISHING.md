# Publishing checklist (for maintainers)

Everything in the repository is ready to publish. These steps need your GitHub account, so they are listed here instead of scripted.

## 1. Create the repository

```bash
cd lanlm
git init -b main
git add .
git commit -m "feat: initial public release"
# create an empty repository on GitHub, then:
git remote add origin https://github.com/<you>/<repo>.git
git push -u origin main
```

## 2. The GitHub address

The project is set up for `https://github.com/sakthiveltofficial/lanlm` (badges, links, `Cargo.toml`, `package.json`, issue templates, workflows). If you publish under a different owner or repository name, change it everywhere with one command, then commit:

```bash
npm run set-repo -- <you>/<repo>
git commit -am "docs: set repository address"
```

If the repository is not called `lanlm`, also adjust the `cd lanlm` lines in `README.md` and `CONTRIBUTING.md`.

## 3. Repository settings

**About box** (the gear next to "About"):

- **Description:** `Run local LLMs on your PC, or pool several PCs to run bigger models. Free Windows desktop app for llama.cpp and GGUF with a cluster mode and an OpenAI-compatible local API.`
- **Website:** leave empty or link the Releases page.
- **Topics** (GitHub allows 20; these are lowercase and hyphenated):
  `llm` `local-llm` `llama-cpp` `gguf` `tauri` `rust` `typescript` `windows` `desktop-app` `offline-ai` `private-ai` `local-ai` `on-device-ai` `distributed-inference` `openai-compatible` `chatgpt-alternative` `self-hosted` `vision-language-model` `ai-chat` `rpc`

**Social preview:** Settings → General → _Social preview_ → upload `docs/images/social-preview.jpg` (1280×640, under GitHub's 1 MB limit). This is the image shown when the link is shared. It is made from `docs/images/banner.png`. To create new artwork with an image generator, use the prompts in [BRANDING.md](BRANDING.md).

**Features to enable** (Settings → General → Features): Issues, Discussions, and Projects if you want a roadmap board.

**Security** (Settings → Code security): turn on **Private vulnerability reporting**, **Dependabot alerts**, **Dependabot security updates** and **Secret scanning**. `SECURITY.md` points reporters to private reporting.

**Labels:** create `good first issue`, `help wanted`, `bug`, `enhancement`, `documentation`, `cluster`, `vision`, `windows`.

## 4. Protect `main`

Settings → Branches → _Add rule_ for `main`:

- Require a pull request before merging (at least 1 approval)
- Require status checks to pass: **CI success** (the single job that gathers the others)
- Require branches to be up to date
- Do not allow force pushes

## 5. Check that CI is green

Open the **Actions** tab. The first run of `CI` and `CodeQL` should pass. The Rust jobs take a while the first time (no cache); later runs are much faster.

The action versions in `.github/workflows` (`actions/checkout@v4`, `actions/setup-node@v4`, `Swatinem/rust-cache@v2`, `tauri-apps/tauri-action@v0`, `github/codeql-action@v3`, …) were written without being able to check them against the latest releases. Dependabot is configured to open pull requests when newer ones exist; if a workflow fails with "unable to resolve action", bump that version.

## 6. Cut a release

1. Update the version in the **three** places (they must match; `npm run check:versions` verifies it): `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`.
2. Move the **Unreleased** notes in `CHANGELOG.md` under the new version and date.
3. Commit, then tag and push:
   ```bash
   git tag v0.1.0
   git push origin v0.1.0
   ```
4. The **Release** workflow runs by itself. It checks the tag matches the version, runs all tests, builds the Windows files, and **publishes a GitHub release** containing:
   - `LanLM_<version>_x64-setup.exe`: the installer
   - `LanLM_<version>_x64_portable.exe`: the single-file portable app
   - `SHA256SUMS.txt`: checksums for both

   A tag with a dash (for example `v0.2.0-beta.1`) is published as a **pre-release**. To rehearse without publishing anything, open **Actions → Release → Run workflow**: it builds everything and keeps the files as a downloadable workflow artifact for 30 days.
5. Afterwards, download the installer from the release page on a clean PC and try it. If something is wrong, edit or delete the release and tag, fix it, and tag again.

### Code signing

The installer is unsigned, so Windows SmartScreen warns about an unknown publisher until the download count builds reputation. To remove the warning you need a code-signing certificate (for example from a certificate authority or Azure Trusted Signing) and to add it to the release workflow as repository secrets. Tauri documents the setup at <https://tauri.app/distribute/sign/windows/>.

## 7. Before announcing

- [ ] Run `npm run verify` one last time
- [ ] Test the installer on a PC that has never had LanLM (engine install, model download, chat)
- [ ] Test a real cluster across two PCs and a vision model end to end, and update the README if anything differs. The screenshots show a worker on the same PC, and vision has not been verified end to end yet.
- [ ] Check each model's license in the catalog
- [ ] Confirm the name "LanLM" is clear for you: a GitHub and web search found no existing project by that name (November 2026), but also check the domain name (`lanlm.com`, `lanlm.dev`), the npm and crates.io names, and a trademark search (USPTO / EUIPO) before you invest in branding
- [ ] Replace "LanLM contributors" in `LICENSE`, `Cargo.toml` and `tauri.conf.json` if you prefer your own name as the copyright holder
