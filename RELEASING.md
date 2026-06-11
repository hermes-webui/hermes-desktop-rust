# Releasing

Tag-driven, fully automated builds. **Pushing a `v*` tag is the trigger** — GitHub
Actions builds macOS (universal DMG), Windows (NSIS `.exe` + `.msi`), and Linux
(AppImage + `.deb`) in parallel and assembles a **draft release** with the matching
CHANGELOG section as the release notes. The only manual step is clicking **Publish**
after a smoke test.

## The whole flow (3 commands + one click)

```bash
# 0. One-time per shell: SSH agent (HTTPS pushes fail for this org)
eval $(ssh-agent -s) && ssh-add ~/.ssh/id_ed25519

# 1. Bump the version everywhere it must agree + seed a CHANGELOG section
scripts/bump_version.sh 0.2.0

# 2. Write the CHANGELOG entry, then commit on main
git add -A && git commit -m "Release v0.2.0"

# 3. Ship it (validates everything, pushes main, then pushes the tag)
scripts/release.sh v0.2.0
```

Then watch CI (`gh run watch --repo hermes-webui/hermes-desktop-rust`), and when all
three platform jobs are green: **Releases → v0.2.0 → Publish release**.

## What the scripts enforce (don't bypass them)

`scripts/release.sh` refuses to ship unless:

- you're on `main` with a clean tree;
- the tag, `src-tauri/tauri.conf.json`, and `src-tauri/Cargo.toml` versions all
  agree (`bump_version.sh` keeps them in sync, plus `package.json` and `Cargo.lock`);
- `CHANGELOG.md` has a `## [vX.Y.Z]` section (it becomes the release notes via
  `scripts/extract_changelog.py`);
- `cargo test` passes locally.

It then pushes `main` and the tag as **two separate operations** — pushing both in
one `git push` sometimes drops one of the two events and the workflow never fires
(inherited lesson from hermes-swift-mac v1.0.5). If a tag ever lands without a
workflow run, trigger manually: Actions → Build and Release → Run workflow, or
re-push the tag.

**"Resource not accessible by integration" on the create-release step** means the
GITHUB_TOKEN is capped to read-only — check BOTH the repo setting (Settings →
Actions → General → Workflow permissions) AND the same setting at the
**organization** level, which silently overrides everything below it (this bit us
on v0.3.0). After fixing the setting, **re-running the failed run is useless** —
re-runs reuse the original run's token privileges. Dispatch a fresh run instead:

```bash
gh workflow run "Build and Release" --repo hermes-webui/hermes-desktop-rust --ref vX.Y.Z
```

## What happens automatically on tag push

`.github/workflows/release.yml`:

1. `changelog` job extracts the `## [vX.Y.Z]` section → release body.
2. Build matrix (all unsigned for now):
   - `macos-14` → universal (arm64+x86_64) `.dmg` + `.app.tar.gz`
   - `windows-2022` → NSIS `.exe` (WebView2 bootstrapper embedded) + `.msi`
   - `ubuntu-22.04` → `.AppImage` + `.deb`
3. Artifacts upload to a **draft** GitHub release named for the tag. Re-running a
   failed job (or re-pushing the same tag) updates the same draft.

`.github/workflows/test.yml` (fmt + clippy `-D warnings` + `cargo test`) runs on
every push/PR to `main` — keep it green so step 3 of a release never surprises you.

## Fixing a botched release (before publishing)

```bash
# fix the problem on main, then move the tag and re-fire:
git push origin :refs/tags/v0.2.0 && git tag -d v0.2.0
git tag v0.2.0 && git push origin v0.2.0
```

The draft release is reused; stale assets are replaced. Never move a tag that has
already been **published** — cut a patch release instead.

## The updater signing key (CRITICAL — do not lose)

Releases are auto-update capable from v0.3.0: tauri-action signs every artifact with
the minisign key in the repo secrets (`TAURI_SIGNING_PRIVATE_KEY` +
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`) and uploads `latest.json`; installed apps
check `releases/latest/download/latest.json` ~10s after launch and via
"Check for Updates…".

- **Losing the private key or password breaks auto-update for every installed
  copy** (users must manually reinstall once with a new key). The key + password
  live in the repo secrets and in the maintainer's `internal/secrets/` working
  folder — back both files up to a password manager.
- The public key is committed in `src-tauri/tauri.conf.json` (`plugins.updater.pubkey`).
- Rotation (if compromised): generate a new pair (`npx tauri signer generate`),
  update pubkey + secrets, ship one release signed with the new key ASAP, and post
  a notice — apps on the old pubkey will reject new updates and need a manual
  download once.
- Coverage: Windows `.exe`/`.msi` and Linux **AppImage** self-update; `.deb`
  installs get a pointer to Releases; macOS `.app` updates in place.
- The draft-release flow still applies: `latest.json` only goes live when the
  release is **published**, so installed apps never see an unsmoked build.

## macOS signing + notarization (ENABLED 2026-06-11)

The release workflow signs with the Developer ID certificate and notarizes via six
repo secrets ([tauri's format](https://tauri.app/distribute/sign/macos/)):
`APPLE_CERTIFICATE` (base64 `.p12` incl. private key), `APPLE_CERTIFICATE_PASSWORD`,
`APPLE_SIGNING_IDENTITY` (`Developer ID Application: Name (TEAMID)`), `APPLE_ID`,
`APPLE_PASSWORD` (app-specific password, `xxxx-xxxx-xxxx-xxxx`), `APPLE_TEAM_ID`.
Hardened runtime is on, with `src-tauri/Entitlements.plist` granting network-client
and microphone (Swift-app parity — voice input breaks under hardened runtime
without it). Notarization adds ~2–5 min to the macOS job.

Gotchas: **every secret must exist and be non-empty** — a present-but-broken
`APPLE_CERTIFICATE` (or a missing password) makes the bundler attempt signing and
fail the whole build. Local `npx tauri build` needs the updater signing key in the
environment (`TAURI_SIGNING_PRIVATE_KEY`/`_PASSWORD` from the maintainer's secure
storage) and produces ad-hoc-signed bundles without Apple identity — entitlements
only embed in CI's signed builds. Windows signing (Azure Trusted Signing / OV cert)
remains config-only when wanted.

## Optional: zero-touch releases

Releases are drafts so unsigned builds get a human smoke test. To make a tag publish
the release with no clicks at all, flip one line in `release.yml`:

```yaml
releaseDraft: false
```
