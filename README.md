# DSH Launcher

English | [中文](https://github.com/Gru110110110/deepseek-harness-desktop-launcher/blob/main/README.zh.md)

DSH Launcher is an unofficial desktop launcher for the published `@deepseek-ai/dsh` package. The desktop application prepares an isolated Node.js/Harness runtime, starts `dsh web`, and opens the exact URL announced by the official service.

The application uses React for presentation, a narrow Tauri command/event adapter, and a reusable Rust core. It does not fork or embed the Harness Web UI.

## Current product scope

- macOS arm64 and x64 DMG installers
- Windows x64 per-user NSIS installer; no portable ZIP distribution
- Pinned Node.js 24.19.0 archives admitted only by platform-specific SHA-256
- Exact `@deepseek-ai/dsh` installation with npm registry fallback
- Transactional staging, executable smoke checks, atomic publication, startup recovery, and rollback
- Browser selection, system tray lifecycle, English/Simplified Chinese, and light/dark/system themes
- Separate Harness updates and cryptographically signed desktop updates

Python/PyInstaller releases do not understand Tauri updater artifacts. Existing users install the first Tauri release manually; it immediately reuses the compatible `~/.dsh-desktop` layout. Later releases are checked in the background and shown before any package is downloaded. After the user confirms, the backend performs the signed download, installation, safe Harness shutdown, and restart as one operation.

## Architecture

```text
React feature registry + HashRouter
  └─ typed launcher API / revisioned state event
      └─ Tauri command and lifecycle adapter
          └─ dsh-core application services
              ├─ runtime deployment and rollback
              ├─ source/CC Switch import
              ├─ managed dsh web process tree
              └─ browser and preferences ports
                  └─ pinned Node.js → published @deepseek-ai/dsh
```

The feature registry owns routes and navigation metadata. Adding future pages means adding a feature descriptor and its backend module, not widening one global view. Business rules live in `dsh-core`, which has no Tauri dependency. Tauri owns only OS lifecycle, tray, clipboard, updater integration, and typed IPC. Commands and events are module-namespaced, and the frontend accepts only monotonically newer snapshot revisions. Windows builds use the GUI subsystem and start every helper process without a console window. When the system tray is available, closing the main window hides it and normal application exit is available from the tray menu. A tray initialization failure does not block startup; in that fallback mode, closing the main window performs a full cleanup and exits. Full exit is not released until deployment and local-service process trees have stopped and the loopback service port has closed. A per-home instance lock prevents concurrent launchers and waits briefly for an updating instance to release ownership. Before starting on macOS or Windows, the launcher verifies executable paths, command arguments, and user ownership, then reclaims every stale service created from its private runtime; macOS additionally verifies process-group ownership. Services run behind a parent-pipe guard that exits after terminating the service when the launcher is killed. Unix guards terminate the isolated process group, while Windows guards combine nested kill-on-close Job Objects with a direct parent-pipe fallback.

The project intentionally avoids a runtime plugin system, generalized workflow engine, or duplicate frontend/backend state machines. Those abstractions are not needed for the current launcher and would make future changes harder rather than easier.

## Data compatibility and safety

The Rust application preserves the existing disk protocol:

```text
~/.dsh-desktop/
├── runtime/{node,dsh,runtime.version,.deployment.lock}
├── cache/
├── dsh-home/
├── server.log
├── install.log
├── server.pid
├── language
├── preferences.json
├── backups/migration-*/dsh-home
├── .migration-complete-v1
└── .migration-skip-v1
```

An explicit `DSH_HOME` disables all imports. Otherwise, the launcher only discovers compatible data in `DSH_DESKTOP_SOURCE_HOME` (default `~/.dsh`) and presents a choice before copying anything. Approval creates and verifies a private backup, performs a restore rehearsal, builds the complete result away from the active home, and publishes it with a crash-recoverable atomic transaction. Skipping is persisted and starts without importing into the existing isolated launcher home. Existing destination values and populated workspace ledgers always win.

CC Switch remains an optional read-only source. The importer opens `cc-switch.db` read-only, accepts only standalone Claude providers with a literal credential, non-loopback HTTP(S) endpoint, supported protocol, and at least one model, and skips OAuth, managed, proxy-dependent, and ambiguous rows. Existing documents are conservatively extended only when their structure is understood. Credentials go only to `.credentials.yaml`, never settings or logs. Two-file publication is rolled back to the exact original bytes on failure. If Windows permissions or another local I/O problem prevents this optional import, the launcher reports that it was skipped and continues installing and starting Harness.

Tests, checks, builds, and packaging must set temporary `DSH_DESKTOP_HOME`, `DSH_HOME`, `DSH_DESKTOP_SOURCE_HOME`, and `DSH_DESKTOP_CC_SWITCH_HOME`. They must never touch real user homes, Keychain, credential stores, or production data.

## Development

Prerequisites: Node.js 24+, pnpm 10.12.3, and Rust 1.96.

```sh
validation_root=$(mktemp -d /tmp/dsh-launcher-dev.XXXXXX)
export DSH_DESKTOP_HOME="$validation_root/desktop"
export DSH_HOME="$validation_root/dsh"
export DSH_DESKTOP_SOURCE_HOME="$validation_root/source"
export DSH_DESKTOP_CC_SWITCH_HOME="$validation_root/cc-switch"

pnpm install --frozen-lockfile
pnpm bindings
pnpm lint
pnpm test
pnpm deadcode
pnpm build
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
pnpm tauri dev
```

`pnpm bindings` generates [bindings.ts](src/platform/generated/bindings.ts) from the Rust domain types. Commit the generated result and keep CI's no-diff check green. `pnpm deadcode` enforces the frontend dependency boundary; strict Clippy does the same for Rust.

The repository-level `public/` directory is the standalone product website. Vite has `publicDir: false`, so the website and desktop assets cannot be accidentally mixed. Website code remains plain HTML/CSS/JavaScript. Cloudflare Workers Builds must use `public` as the root directory, no build command, and `npx wrangler deploy` as the deploy command. The Worker proxies the published GitHub updater manifest at `/latest.json`; installed clients try this endpoint first and fall back to GitHub directly. Update packages and their mandatory signatures remain GitHub Release assets. `pnpm cloudflare:check` guards this contract and all local website assets without changing the existing automatic deployment path.

## Release and signing

All versions in `package.json`, workspace `Cargo.toml`, and `src-tauri/tauri.conf.json` must match the `desktop-v<version>` tag. `pnpm versions` enforces this rule.

Tauri updater signatures are mandatory even when platform signing is unavailable:

1. Create the Git-ignored local key directory and generate the updater key pair with an explicit **file** path (the `-w` target is not a directory):

   ```sh
   mkdir -p signer-keys
   chmod 700 signer-keys
   pnpm tauri signer generate -w signer-keys/dsh-launcher-updater.key
   ```

   Never use `--force` on an existing updater key without a separately reviewed rotation and recovery plan. Never commit the private key, and keep a verified encrypted backup outside the repository.

2. Store the private key and optional password in GitHub secrets `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
3. Store the complete contents of `signer-keys/dsh-launcher-updater.key.pub` in the GitHub Actions variable `TAURI_UPDATER_PUBLIC_KEY`.
4. Push `desktop-v<version>`. CI validates both values, verifies the pinned Node/npm transports, then builds and signs isolated macOS arm64/x64 and Windows x64 artifacts without publishing from matrix jobs. One final job stages versioned, architecture-specific names, creates a clean draft GitHub Release, uploads all assets serially, verifies every installer, updater archive, signature, manifest entry, and exact website download URL, and only then publishes the verified release. A failed or incomplete build remains unpublished, so `releases/latest/download/latest.json` and installed clients never observe a partial release.

The checked-in updater public key is intentionally empty: local source builds do not belong to a production update channel. Release CI validates the configured minisign public key, writes a temporary release-only Tauri config, and passes it explicitly to the CLI with `--config`; a release cannot build without both sides of the updater trust chain. In the absence of a Developer ID, macOS bundles receive a complete ad-hoc signature and both local packaging and CI reject bundles that fail strict `codesign` verification. Ad-hoc signing does not provide Apple notarization: a browser-downloaded build may still require the user to approve its first launch in macOS Privacy & Security. A warning-free first launch on arbitrary Macs requires a Developer ID Application certificate and notarization. Windows Authenticode remains optional independent hardening and does not weaken the mandatory Tauri update signature.

## Runtime environment overrides

| Variable                     | Meaning                                                                                                                      |
| ---------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `DSH_DESKTOP_HOME`           | Launcher/runtime home; defaults to `~/.dsh-desktop`                                                                          |
| `DSH_HOME`                   | Explicit external Harness home; bypasses the isolated desktop `dsh-home` and disables all imports. Set it only deliberately. |
| `DSH_DESKTOP_SOURCE_HOME`    | Optional source-home import; defaults to `~/.dsh`                                                                            |
| `DSH_DESKTOP_CC_SWITCH_HOME` | Optional read-only CC Switch source; defaults to `~/.cc-switch`                                                              |
| `DSH_DESKTOP_NODE_VERSION`   | Exact Node override; requires `DSH_DESKTOP_NODE_SHA256`                                                                      |
| `DSH_DESKTOP_NODE_SHA256`    | SHA-256 trust root for an overridden Node archive                                                                            |
| `DSH_DESKTOP_NODE_BASES`     | Comma-separated Node mirrors; explicit values suppress defaults                                                              |
| `DSH_DESKTOP_NPM_REGISTRIES` | Comma-separated npm registries; explicit values suppress defaults                                                            |

## License

The launcher source is MIT-licensed. `@deepseek-ai/dsh`, Node.js, Tauri, React, and other dependencies retain their own licenses and terms.
