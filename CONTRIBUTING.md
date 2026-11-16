# Contributing

Thanks for considering a contribution!

## Getting started

1. Fork the repo and clone your fork.
2. Install Rust, Node 22 and pnpm. On Ubuntu, also install the system
   libraries listed in `.github/workflows/ci.yml` (the `apt-get install` line).
3. Install dependencies and run the app:

   ```bash
   pnpm install
   pnpm tauri dev
   ```

   The first dev build compiles the whole Rust backend, which takes 5–10 minutes.
   After that, frontend changes hot-reload instantly.

## Before you open a PR

- `pnpm build` passes (TypeScript + Vite production build).
- `cargo test` passes when run from inside `src-tauri/`. It also regenerates
  `src/lib/bindings/`; commit those files with your change.
- Keep changes focused — one topic per pull request.
- Read [ARCHITECTURE.md](ARCHITECTURE.md), [GLOSSARY.md](GLOSSARY.md) and
  the ADRs in `docs/adr/` before touching the engine or a platform Backend.

## Pull requests

PRs target `main`. The branch is protected: a pull request needs one
approving review (from the code owner) and is merged by squash or rebase to
keep the history linear. For bigger features, please open an issue first so
the approach can be discussed before you invest time in code.

## Reporting bugs

Open an issue with your OS and desktop session (e.g. Ubuntu 24.04, GNOME
Wayland), the app version and the steps to reproduce. Don't use issues for
security vulnerabilities — see [SECURITY.md](SECURITY.md).
