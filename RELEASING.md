# Releasing — maintainer guide

How to publish a new version of Clipboard History. Same pipeline as
prayer-times: GitHub Actions builds every platform when a `v*` tag is pushed.

> **TL;DR** — bump the version in 3 files → commit → push → check CI is green
> → tag → push the tag. Done.

## One-time setup (do once, before the first release)

1. **Generate the updater signing keypair** (pick a password and keep it):

   ```bash
   pnpm tauri signer generate -w ~/.tauri/clipboard-history.key
   ```

   This writes `~/.tauri/clipboard-history.key` (private) and
   `~/.tauri/clipboard-history.key.pub` (public). **Back up the private key
   and its password somewhere safe** (password manager).

2. **Put the public key in the app:** copy the whole content of
   `~/.tauri/clipboard-history.key.pub` into `src-tauri/tauri.conf.json` →
   `plugins.updater.pubkey` (replacing `REPLACE_WITH_…`). Commit it.

3. **Add the repository secrets:** GitHub → repo → Settings → Secrets and
   variables → Actions → New repository secret:

   | Name | Value |
   | ---- | ----- |
   | `TAURI_SIGNING_PRIVATE_KEY` | entire content of `~/.tauri/clipboard-history.key` |
   | `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | the password from step 1 |

4. **Make the repo public:** Settings → General → Danger Zone → Change
   visibility → Public. (The in-app updater downloads `latest.json` from
   GitHub Releases without logging in, so the releases must be public.
   Free accounts also only get enforced rulesets on public repos.)

5. **Protect `main`:** Settings → Rules → Rulesets → New ruleset → Import a
   ruleset → pick `.github/rulesets/main-branch.json`. Check that your user
   (`iamraihan`) is in its Bypass list so you can push release bumps
   directly.

6. **Turn on private vulnerability reporting** (`SECURITY.md` points to it):
   Settings → Advanced Security (or "Code security") → Private vulnerability
   reporting → Enable.

7. **Allow Actions to write releases:** Settings → Actions → General →
   Workflow permissions → "Read and write permissions".

⚠️ **Never lose the private key.** Every installed app only trusts updates
signed with it. If it's lost, all existing installs lose auto-updates
forever and must reinstall manually.

## Every release, step by step

Say the next version is `0.2.0`:

1. **Bump the version to `0.2.0` in all three files** (keep them identical):

   | File | Field |
   | ---- | ----- |
   | `src-tauri/tauri.conf.json` | `"version"` ← the one the updater reads |
   | `src-tauri/Cargo.toml` | `version` |
   | `package.json` | `"version"` |

   Quick check: `grep '"version"' package.json src-tauri/tauri.conf.json && grep '^version' src-tauri/Cargo.toml`

   Then run `cargo check --manifest-path src-tauri/Cargo.toml` once so
   `Cargo.lock` records the new version too (CI runs `cargo test --locked`).

2. **Run the manual desktop checklist** in `.claude/skills/desktop-test/SKILL.md`
   on GNOME X11 and Wayland.

3. **Commit and push:**

   ```bash
   git add -A
   git commit -m "release v0.2.0"
   git push
   ```

4. **Wait for CI to go green** (Actions tab → "CI"). Never tag a red build.

5. **Tag and push the tag — the tag number must match the files:**

   ```bash
   git tag v0.2.0
   git push origin v0.2.0
   ```

6. **Watch the Release workflow** (Actions tab → "Release"). Four parallel
   builds run: Windows, Ubuntu, macOS (Apple Silicon + Intel). Takes ~15–20 min.

7. **Check the result** on the Releases page: installers for all platforms,
   plus the signed update files and `latest.json` that installed apps check.

Installed apps check for updates on launch and once a day. When one is
found, the tray menu shows **Update to vX.Y.Z and restart** (Windows, Linux).
On macOS it shows **Download vX.Y.Z…** and opens the release page instead,
because silent updates there need a paid Apple Developer ID certificate.
Users can also check by hand from the tray ("Check for updates (vX.Y.Z)").
Debug builds (`pnpm tauri dev`) never check on their own, only when you
click that tray line.

## Choosing the version number

| Bump | Use when | Example |
| ---- | -------- | ------- |
| Patch `0.2.0 → 0.2.1` | bug fix, small tweak | fixed paste on X11 |
| Minor `0.2.0 → 0.3.0` | new feature, settings option | added excluded apps |
| Major `0.x → 1.0` | big milestone / breaking change | first "stable" release |

## Troubleshooting

| Symptom | Cause | Fix |
| ------- | ----- | --- |
| `git push` to main rejected (`GH006: Protected branch`) | Your user is missing from the ruleset bypass list | Settings → Rules → Rulesets → edit ruleset → Bypass list → add `iamraihan` |
| Release workflow fails at signing (`missing private key` / env var error) | `TAURI_SIGNING_PRIVATE_KEY` secret missing or wrong content | Re-add the secret; content must be the **entire** `~/.tauri/clipboard-history.key` file |
| Release fails with `Resource not accessible by integration` | Actions can't write releases | One-time setup step 7 |
| Workflow creates a different tag than you pushed | Tag number ≠ version in `tauri.conf.json` | Delete the tag (below), fix files, re-tag |
| Tagged by mistake / want to redo | — | `git tag -d v0.2.0` (local), `git push origin :refs/tags/v0.2.0` (remote), delete the release on GitHub if it was created, then fix and re-tag |
| Linux build fails: a missing system library (`Package xcb-xfixes was not found`, …) | A system dev package missing from the workflow's `apt-get install` list | Add the package in **both** `.github/workflows/release.yml` and `ci.yml`, commit, then delete and re-push the tag (re-runs use the old workflow file) |
| Installed app never offers the update | `pubkey` in `tauri.conf.json` doesn't match the signing key, or the repo/release is private | Fix the key (setup step 2) or make the repo public; a build with a wrong key must be reinstalled by hand |
| `pnpm tauri build` fails locally about a signing key | `createUpdaterArtifacts` needs the private key | `export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/clipboard-history.key)"` and the password variable, or just let CI build |
| CI red | Code problem | Fix and push **before** tagging; never release on red |

## Notes

- Tags point at a commit — the version bump commit is what gets tagged, so
  always **tag last**, after pushing the bump.
- The Release workflow (`.github/workflows/release.yml`) only triggers on tags
  matching `v*`. CI (`.github/workflows/ci.yml`) runs on every push to main
  and every PR.
- Only Linux has a working clipboard Backend so far; the macOS and Windows
  installers start but don't watch the clipboard until those Backends land.
