# Security Policy

## Reporting a vulnerability

Please **do not open a public issue** for security problems.

Report privately through GitHub: go to the repository's **Security** tab → **Report a vulnerability** ([direct link](https://github.com/sakthiveltofficial/lanlm/security/advisories/new)). Include:

- what the problem is and which part of LanLM it affects
- steps to reproduce, or a proof of concept
- the version (the release tag or installer file name) and your Windows version

We will acknowledge your report within a few days, keep you updated, and credit you in the release notes if you wish. Please give us a reasonable time to fix the issue before disclosing it publicly.

## Supported versions

Security fixes are made to the **latest released version**. Please update before reporting.

## Things to know about how LanLM works

LanLM is a desktop app that starts local helper programs. These are the points that matter for security:

| Area | Behaviour | Advice |
| --- | --- | --- |
| **Cluster (mesh)** | PCs authenticate each other with a shared cluster key and talk over encrypted links (Noise `XXpsk3`). A new PC joins only after a person presses **Allow** and both screens show the same six-digit code. llama.cpp workers listen on the local machine only and are reached through the encrypted tunnel. **Members trust each other:** a PC that holds the cluster key can read prompts that reach it and can start work on other members. Models copied between PCs are checked against a hash. | Share the cluster key only with PCs you trust, keep the cluster port (50500) off the internet, and use **Leave cluster** on a PC you give away. |
| **Cluster workers (older method)** | A worker runs llama.cpp's RPC server, which has **no authentication or encryption**. Anyone who can reach its port can use that PC's CPU and memory and send it data. | Start a worker only on a trusted network, leave **Allow other PCs** off unless you need it, and stop the worker when you are done. Do not expose its port to the internet. |
| **API server** | Off by default. When on, it listens on this PC only. **Allow other devices on my network** requires an API key. | Use a long random key (**Create** makes one). Do not expose the port to the internet. |
| **Downloads** | The engine comes from the official llama.cpp GitHub releases, and models from the Hugging Face links you choose, over HTTPS. LanLM does not yet verify checksums or signatures of what it downloads. | Download models from sources you trust. |
| **Installer** | Not code-signed yet, so Windows SmartScreen warns about it. | Build from source or compare with the public GitHub Actions build. |
| **Local data** | Chats and settings are stored in the app's local web storage on your PC, unencrypted, including the API key if you set one. | Use Windows account protection and disk encryption for sensitive material. |
| **Model output** | Replies are escaped before they are shown, so a model cannot inject HTML or scripts into the interface. | Report any bypass you find. |
| **File names** | Model file names from the interface are validated so they cannot escape the models folder. | Report any bypass you find. |

## Out of scope

Reports about vulnerabilities in llama.cpp itself, Hugging Face, or the models should go to those projects. Findings that require an already-compromised machine or physical access are generally out of scope.
