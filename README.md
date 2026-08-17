# DSH Launcher

English | [中文](README.zh.md)

An unofficial desktop launcher for DeepSeek Harness. Double-clicking it downloads and starts the DeepSeek Harness service automatically, then lets the user open the Web UI address announced by the service in an installed browser. Closing the desktop window hides it to the system tray while the service keeps running; the tray Quit command performs the full shutdown. The launcher UI supports Simplified Chinese and English on macOS and Windows, with packages built by GitHub Actions.

DSH Launcher downloads the upstream `@deepseek-ai/dsh` package at runtime. That package is upstream software and remains subject to its own license and terms. This repository's MIT License covers only the DSH Launcher code and does not relicense `@deepseek-ai/dsh`, DeepSeek Harness, or any DeepSeek trademarks or brand assets.

## How it works

The launcher is a thin shell. Its release pins one verified Node archive. On first installation it resolves the current Harness version from the configured registries and freezes that result as an exact deployment target. Every launch validates the installed manifests and executable entry points, while an online check offers only a strictly newer semantic version:

```text
启动
  ├─ 校验版本标记、包 manifest、Node 与 dsh 可执行入口
  ├─ 已就绪 → 直接启动服务；未就绪 → 从缓存或多源下载准备已验证版本
  ├─ SHA-256 校验 → staging 安装与 smoke → 原子替换；失败保留旧版本
  ├─ 启动 node dsh/node_modules/@deepseek-ai/dsh/lib/bin.js web
  ├─ 官方默认端口被占用 → 用官方 --port 0 自动选择空闲端口后重试
  ├─ 检查 npm 上 @deepseek-ai/dsh 是否有新版本，有则提示「立即更新」
  └─ 收到 dsh web: <URL> → 显示实际地址 + 运行时长 + 「打开 Web UI」按钮
桌面窗口
  ├─ 显示：品牌侧栏、版本号、运行时长、服务状态（两步骤）
  ├─ 语言：首次按系统首选语言选择中文或英文，侧栏菜单可随时切换
  ├─ 单款浏览器 → 直接显示打开按钮；多款浏览器 → 下拉选择后打开
  ├─ 「打开 Web UI」→ 所选浏览器打开服务发布的实际地址
  └─ 关闭窗口 → 隐藏到系统托盘，依赖准备与本地服务继续运行
系统托盘
  ├─ 「显示启动主页面」→ 恢复并聚焦启动器窗口
  ├─ 「打开Web UI」→ 所选浏览器打开服务发布的实际地址
  └─ 「退出」→ 终止依赖准备进程与整个服务进程树
```

On first installation, the launcher concurrently queries valid `latest` metadata from the configured registries, selects the highest SemVer, and freezes that result for the deployment. Node archives come from bounded, resumable transports and must match the release's SHA-256; npm installs the selected exact Harness version through the first registry that proves it reachable. Node transport alternates between the official endpoint and npmmirror on retry, while npm probes both registries concurrently; any explicit source list is authoritative. Node transfer shows byte-derived percentage, while metadata, extraction, npm installation, validation, activation, and service startup show the exact activity and a live elapsed timer instead of a fabricated percentage. Choosing “Update now” stages its already selected exact candidate, validates it, atomically switches directories, and restarts the service; failure restores the previous directory and marker. Runtime data uses this layout:

```text
~/.dsh-desktop/                    # 可用 DSH_DESKTOP_HOME 覆盖
├── cache/
│   ├── node-v*                    # 已校验的 Node.js 归档与断点文件
│   └── npm/                       # 可跨重试复用的 npm 内容缓存
├── runtime/
│   ├── node/                      # Node.js 发行版（含 npm）
│   ├── dsh/node_modules/          # @deepseek-ai/dsh 及其依赖
│   ├── node.previous/ / dsh.previous/ # 最近一次可回滚目录
│   ├── .deployment.lock           # 跨进程部署锁
│   └── runtime.version            # 原子写入的 harness 版本标记
├── dsh-home/                      # dsh web 的会话/配置（隔离自 ~/.dsh）
├── language                       # 桌面启动器的显式语言选择（zh 或 en）
├── .source-home-import-v1         # 已完成一次源码 home 迁移
├── .source-workspace-import-v1    # 已完成一次工作区分组迁移
├── .cc-switch-import-v2           # 已完成一次兼容 CC Switch 供应商合并
├── server.log                     # dsh web 服务日志
└── install.log                    # npm 安装日志（安装失败时排查用）
```

By default, service data is isolated under `~/.dsh-desktop/dsh-home`. When `.source-home-import-v1` is absent, or whenever that home has no configuration, the launcher imports missing configuration entries from `~/.dsh`; existing files win, while existing directories receive only missing descendants. It copies `sessions` and `attachments` as one history unit only when neither destination directory exists. A separate `.source-workspace-import-v1` snapshot imports a compatible workspace v2 grouping ledger from `storages/workspace.json`: a missing destination receives it, while a validated initialized ledger with no workspaces or archived sessions can be repaired from it; a populated or unrecognized desktop ledger always wins. Both completion markers prevent later launches from synchronizing newly added source data. Every other `storages` file, the anonymous user id, installed `node_modules`, transient writer files, and symbolic links remain excluded.

After those snapshots, the launcher makes exactly one optional CC Switch decision and records it in `.cc-switch-import-v2`, whether the database is absent, unreadable, skipped, or successfully imported. Version 2 replaces the earlier preserve-whole-document decision with a conservative missing-only merge, so a v1 marker does not suppress this corrected one-time pass. On that sole check it opens `~/.cc-switch/cc-switch.db` in SQLite read-only mode. Only CC Switch's self-contained Claude Code providers with a non-loopback HTTP(S) endpoint, a literal API key, at least one model, and a DSH-supported Anthropic Messages, OpenAI Chat Completions, or OpenAI Responses protocol are translated. OAuth/managed-account providers, `PROXY_MANAGED` entries, local-routing endpoints, full-URL overrides, unsupported formats, and incomplete records are skipped. Existing compatible JSON documents are deep-added only with missing routes and credential references; a conservatively recognized YAML mapping can receive a missing `llm-pi-ai` section and new credential keys without rewriting its existing text. Every existing value wins, while an unfamiliar structure or conflicting route is preserved and skipped. Candidate `settings.yaml` and `.credentials.yaml` documents are staged and validated before atomic publication with owner-only file permissions; a partial publication restores the exact original bytes, secrets never enter settings or logs, and every later launch skips CC Switch without reopening its database. An explicit `DSH_HOME` disables every import. The one-time decision is appended to `server.log` without configuration contents.

When `language` has no valid saved selection, the launcher reads the operating system's ordered UI-language preferences and chooses the first shipped primary language (`zh` or `en`), falling back to Chinese when neither is present. The sidebar language menu applies immediately and stores the explicit selection atomically. This launcher preference is separate from the Web UI language stored under the Harness home.

**Installation troubleshooting:** npm installation writes to `~/.dsh-desktop/install.log`. The launcher probes configured registries concurrently, tries reachable sources first, and bounds the complete install interval. A failed attempt never deletes the active runtime. Inspect the log for each source's npm output; Node transport and checksum failures appear directly in the launcher. Closing the window keeps the owned deployment process running in the tray; use the tray Quit command to cancel and join it.

## Directory layout

```text
deepseek-harness-desktop/
├── app_paths.py             # 名称、版本、数据目录
├── localization.py          # 系统语言检测、语言偏好与双语桌面文案
├── browser_manager.py       # 浏览器发现与指定浏览器启动
├── tray_manager.py          # macOS/Windows 系统托盘菜单
├── runtime.py               # 有界下载、校验、事务安装、回滚与版本检查
├── release_check.py         # 打包前验证 Node 固定摘要与 npm latest 元数据
├── server_manager.py        # dsh web 进程的启动与终止
├── main.py                  # tkinter 状态窗口（--check 可无 GUI 自检）
├── requirements-runtime.txt # pystray、Pillow 与平台桥接依赖
├── requirements-build.txt   # PyInstaller
├── THIRD_PARTY_NOTICES.md   # 打包依赖的许可证说明
├── build/
│   ├── mac.spec             # macOS .app 打包配置
│   ├── windows.spec         # Windows onedir packaging (avoids self-extraction)
│   ├── package_windows.ps1  # Creates the distributable Windows ZIP
│   ├── sign_windows.ps1     # Optional Authenticode signing in CI
│   ├── create_macos_dmg.sh  # 制作 DMG
│   └── generate_icons.py    # Generate checked-in app icons (maintenance only)
├── tests/                   # 非 GUI 模块的单元测试
├── assets/                  # 应用图标
└── build-local.sh / .bat    # 本地打包脚本
```

## Local builds

Python 3.11 with Tk is required. The build scripts install the pinned tray and packaging dependencies in `.build-venv`.

macOS (produces an `.app` and DMG):

```sh
# brew install python-tk@3.11   # 如缺 tkinter
./build-local.sh
```

Windows (produces `DSHLauncher-Windows-x64.zip`):

```bat
build-local.bat
```

Artifacts are written to `dist/`. The runtime (Node and dsh) downloads and installs automatically on first launch, so local builds do not need to bundle it.

## GitHub Actions packaging

Workflow: `.github/workflows/desktop.yml`.

- **Manual:** On the Actions page, choose *Build Desktop App* → *Run workflow*. Packages are uploaded as artifacts without creating a Release.
- **Release:** Pushing a `desktop-v*` tag, such as `desktop-v0.1.1`, runs the desktop suite, verifies the pinned Node metadata and valid Harness `latest` metadata through the official and npmmirror transports, builds macOS (arm64/x64) and Windows (x64), smokes each packaged launcher through `--check`, generates `SHA256SUMS.txt`, and creates a GitHub Release.

## Release process

1. Update `APP_VERSION` in `app_paths.py`; it determines the `desktop-v<APP_VERSION>` Release tag. When changing `NODE_VERSION`, update every supported archive hash in `runtime.py` from the signed Node release manifest in the same change. Packaging fails when either default Node manifest does not match the pinned archive or either default npm registry does not return valid Harness `latest` metadata.
2. Create and push the tag:

   ```sh
   git tag desktop-v0.1.1
   git push origin desktop-v0.1.1
   ```

3. Download the platform package (`.dmg` or the Windows `.zip`) and `SHA256SUMS.txt` from the Release and distribute them together.

## Tests

```sh
python3.11 -m unittest discover -s tests -v
```

## Optional environment variables

| Variable | Purpose |
| --- | --- |
| `DSH_DESKTOP_HOME` | Overrides the data directory (default: `~/.dsh-desktop`) |
| `DSH_DESKTOP_SOURCE_HOME` | Overrides the optional one-time import source (default: `~/.dsh`); use an isolated path for tests/build checks |
| `DSH_DESKTOP_CC_SWITCH_HOME` | Overrides the optional read-only CC Switch source directory (default: `~/.cc-switch`); required when CC Switch uses a custom data directory and for isolated tests |
| `DSH_DESKTOP_NODE_BASES` | Comma-separated Node distribution bases; an explicit list suppresses public defaults |
| `DSH_DESKTOP_NODE_BASE` | One authoritative Node distribution base when the plural form is absent |
| `DSH_DESKTOP_NODE_VERSION` | Overrides the exact pinned Node version; requires `DSH_DESKTOP_NODE_SHA256` |
| `DSH_DESKTOP_NODE_SHA256` | Trusted SHA-256 for an overridden Node archive |
| `DSH_DESKTOP_NPM_REGISTRIES` | Comma-separated npm registries; an explicit list suppresses public defaults |
| `DSH_DESKTOP_NPM_REGISTRY` | One authoritative npm registry when the plural form is absent |
| `DSH_DESKTOP_NETWORK_TIMEOUT_SECONDS` | Connect and idle-read timeout for one HTTP operation (default: 10) |
| `DSH_DESKTOP_DOWNLOAD_TIMEOUT_SECONDS` | Total Node download interval across retries and sources (default: 600) |
| `DSH_DESKTOP_INSTALL_TIMEOUT_SECONDS` | Total npm install interval across registries (default: 900) |

## Notes

- **Windows antivirus compatibility:** Windows is built as a PyInstaller one-folder application and distributed as `DSHLauncher-Windows-x64.zip`. This removes the one-file bootloader's runtime self-extraction, a common heuristic false-positive trigger. Extract the complete `DSHLauncher` folder before running `DSHLauncher.exe`; do not move the EXE away from its `_internal` directory.
- **Optional Windows signing:** Releases remain buildable without a certificate. To Authenticode-sign the launcher in GitHub Actions, configure repository secrets `WINDOWS_SIGNING_CERT_BASE64` (the Base64-encoded PFX) and `WINDOWS_SIGNING_CERT_PASSWORD`; the workflow signs with SHA-256, adds an RFC 3161 timestamp, and verifies the signature before creating the ZIP. `WINDOWS_TIMESTAMP_URL` may be set as a repository variable to override the default timestamp service.
- **macOS signing:** macOS remains independently buildable with ad-hoc signing; first launch may require right-clicking the app and choosing “Open” or following the system prompt. Developer ID signing and notarization are optional future hardening, not a build requirement. macOS does not use the Windows one-file bootloader and therefore does not need the Windows packaging change.
- **macOS architecture builds:** Apple Silicon packages use the `macos-15` runner and Intel packages use `macos-15-intel`. CI and the local build script verify the packaged launcher's Mach-O architecture with `lipo` before creating or uploading the DMG.
- **Runtime deployment:** Official Node/npm endpoints and npmmirror serve as default transports, but SHA-256 and exact versions determine admitted content. Partial Node downloads and npm's content cache survive retry. One cross-process lock serializes writers; staging directories, executable smokes, atomic markers, retained previous directories, and startup recovery prevent an interrupted or failed update from replacing the last valid runtime. The npm subprocess receives proxy and certificate settings but not ambient API keys, passwords, tokens, or the user's npm configuration.
- **Address discovery:** The launcher first uses the official default address. Only when the service reports `EADDRINUSE` does it retry with the official `--port 0` option so the operating system selects a free loopback port. The official `dsh web: <URL>` output remains the readiness signal and sole displayed and opened address. If the service exits for another reason or does not announce an address within 60 seconds, the launcher stops it and reports the failure; it never substitutes a desktop-owned host or port.
- **Browser selection:** The launcher detects common installed browsers when it starts. One detected browser keeps a single open button; several add a browser menu beside it. If no known browser is found, the system default remains available as the sole fallback. The logo opens `https://dsdesktop.com` in the selected browser, and the published Web UI address can be clicked to copy it. An explicit browser receives each URL as a separate process argument without invoking a shell.
- **System tray lifecycle:** The tray icon is present for the launcher lifetime. Closing the window only hides it. “Show Launcher” restores it, “Open Web UI” uses the currently selected browser and is disabled until the service publishes an address, and “Quit” cancels deployment, stops the complete service process tree, and exits. On macOS, Command-Q follows the native quit behavior. If tray startup fails, the window remains recoverable and closing it falls back to a full exit.
- **Launcher language:** A valid saved `language` value wins over operating-system preferences. Without one, macOS reads `AppleLanguages`, Windows reads the user's preferred UI languages, and other systems use standard locale environment values. Language switching updates the standing launcher state, including deferred errors, without restarting the service; raw third-party error details remain unchanged.
- **Data isolation and first-run import:** The desktop service uses `~/.dsh-desktop/dsh-home`. Versioned markers make source configuration/history, workspace grouping, and compatible CC Switch Claude providers separate one-time snapshots. Missing source-home configuration descendants can join existing desktop directories without replacing any path; source history copies only when the desktop has neither `sessions` nor `attachments`. The only imported storage-domain file is a compatible workspace v2 ledger, and only a missing or validated empty desktop ledger accepts it; all other mutable storage, dependency installations, and symbolic links remain excluded. CC Switch is opened read-only and contributes only independently usable provider fields through a conservative missing-only merge; existing values, OAuth state, and local-routing state stay behind. The imported on-disk formats must be readable by the desktop Harness version at import time; after the snapshots, each home may diverge. Migrated profiles that depend on external plugins need those dependencies installed separately in the desktop home. `server.log` records each import outcome and reason without secrets.
