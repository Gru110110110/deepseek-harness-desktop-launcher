# DSH Launcher

DSH Launcher 是已发布 `@deepseek-ai/dsh` 包的非官方桌面启动器。桌面应用负责准备隔离的 Node.js/Harness 运行环境、启动 `dsh web`，并打开官方服务实际发布的 URL。

应用以 React 负责表现层，以窄接口的 Tauri 适配层负责系统能力，以可复用的 Rust 核心负责业务规则。它不 fork、也不内嵌 Harness Web UI。

## 当前产品范围

- macOS arm64 与 x64 DMG 安装包
- Windows x64 按用户安装的 NSIS 安装包；不再发布便携 ZIP
- 固定 Node.js 24.19.0，只有平台专属 SHA-256 匹配的归档才能进入运行环境
- 精确安装 `@deepseek-ai/dsh`，支持 npm registry 回退
- staging、可执行 smoke 校验、原子发布、启动恢复与失败回滚
- 浏览器选择、系统托盘生命周期、中英双语、浅色/深色/跟随系统主题
- Harness 更新与带密码学签名的桌面应用更新相互独立

Python/PyInstaller 版本无法识别 Tauri 更新产物。老用户需要手动安装第一版 Tauri 应用；新应用会直接复用兼容的 `~/.dsh-desktop` 目录。此后只在后台检查并先提示新版本，不会提前下载。用户确认后，后端会连续完成签名包下载、安装、安全停止 Harness 与重启。

## 架构

```text
React 功能注册表 + HashRouter
  └─ 类型化 launcher API / 带 revision 的状态事件
      └─ Tauri 命令与生命周期适配层
          └─ dsh-core 应用服务
              ├─ 运行环境部署与回滚
              ├─ source/CC Switch 导入
              ├─ 托管 dsh web 进程树
              └─ 浏览器与偏好设置端口
                  └─ 固定 Node.js → 已发布 @deepseek-ai/dsh
```

功能注册表统一拥有路由和导航元数据。后续增加页面时只需增加 feature descriptor 和对应后端模块，不必继续扩大单个全局 View。业务规则全部位于不依赖 Tauri 的 `dsh-core`；Tauri 只负责操作系统生命周期、托盘、剪贴板、更新器和类型化 IPC。命令与事件按模块命名，前端只接受 revision 单调递增的新状态。

项目有意不引入运行时插件系统、通用工作流引擎或前后端重复状态机。这些抽象对当前启动器没有收益，只会增加未来修改成本。

## 数据兼容与安全

Rust 应用保持原有磁盘协议：

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

显式 `DSH_HOME` 会关闭所有导入。否则启动器只会在 `DSH_DESKTOP_SOURCE_HOME`（默认 `~/.dsh`）中发现兼容数据，并在复制任何内容前要求用户选择。确认导入后会创建并校验私有备份、完成恢复演练、在活动目录之外构建完整结果，再通过可从崩溃恢复的原子事务发布；选择跳过会被持久化，并在不导入来源数据的情况下使用现有隔离启动器目录启动。已有目标值和已填充的 workspace ledger 始终优先。

CC Switch 只是可选的只读来源。导入器以只读方式打开 `cc-switch.db`，只接受具有字面凭据、非回环 HTTP(S) 地址、受支持协议且至少包含一个模型的独立 Claude provider；OAuth、托管账号、依赖代理和含义不明确的记录全部跳过。只有在能可靠理解既有文档结构时才补充缺失值。凭据只进入 `.credentials.yaml`，永不进入 settings 或日志。双文件发布失败时会恢复为完全一致的原始字节。

测试、检查、构建和打包必须设置临时的 `DSH_DESKTOP_HOME`、`DSH_HOME`、`DSH_DESKTOP_SOURCE_HOME` 与 `DSH_DESKTOP_CC_SWITCH_HOME`。不得接触真实用户目录、Keychain、凭据存储或生产数据。

## 开发

依赖：Node.js 24+、pnpm 10.12.3、Rust 1.96。

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

`pnpm bindings` 从 Rust 领域类型生成 [bindings.ts](src/platform/generated/bindings.ts)。生成结果需要提交，CI 会检查重新生成后没有差异。`pnpm deadcode` 约束前端依赖边界，严格 Clippy 对 Rust 执行同类约束。

仓库根目录的 `public/` 是独立官网。Vite 配置了 `publicDir: false`，官网与桌面资源不会意外混入彼此。官网代码继续使用原生 HTML/CSS/JavaScript。Cloudflare Workers Builds 应将根目录设为 `public`、构建命令留空、部署命令设为 `npx wrangler deploy`。Worker 会在 `/latest.json` 代理已发布的 GitHub 更新清单；客户端优先请求此端点，失败后直接回退 GitHub。更新包及其强制签名仍是 GitHub Release 产物。`pnpm cloudflare:check` 会守护这一约定与全部本地资源，且不改变现有自动部署路径。

## 发布与签名

`package.json`、workspace `Cargo.toml` 与 `src-tauri/tauri.conf.json` 的版本必须和 `desktop-v<version>` tag 一致，`pnpm versions` 会强制检查。

即使平台代码签名暂未配置，Tauri 更新签名也必须存在：

1. 创建已被 Git 忽略的本地密钥目录，并使用明确的**文件**路径生成更新密钥对（`-w` 的目标不是目录）：

   ```sh
   mkdir -p signer-keys
   chmod 700 signer-keys
   pnpm tauri signer generate -w signer-keys/dsh-launcher-updater.key
   ```

   未经单独评审的轮换和恢复方案，不得对已有更新密钥使用 `--force`。私钥绝不能提交，并应在仓库外保留经过验证的加密备份。
2. 将私钥和可选密码保存为 GitHub secrets：`TAURI_SIGNING_PRIVATE_KEY`、`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。
3. 将 `signer-keys/dsh-launcher-updater.key.pub` 的完整内容保存为 GitHub Actions variable：`TAURI_UPDATER_PUBLIC_KEY`。
4. 推送 `desktop-v<version>`。CI 会验证两端配置，核对固定 Node/npm 来源，构建 macOS arm64/x64 与 Windows x64，为更新产物签名，并创建 GitHub Release 草稿。最后由唯一一个 job 汇总并验证三个平台条目，再上传 `latest.json`。

草稿是有意保留的人工发布门禁。检查安装包与 `latest.json` 后，需要在 GitHub 中发布该草稿。在正式发布前，GitHub 的 `releases/latest/download/latest.json` 端点不会暴露它，已安装客户端也无法发现这次更新。

仓库配置中的 updater 公钥有意留空：本地源码构建不属于生产更新频道。发布 CI 会校验 minisign 公钥格式、写入仅用于本次发布的临时 Tauri 配置，并通过 `--config` 显式交给 CLI；更新信任链任一端缺失都会阻止发布。没有 Developer ID 时，macOS App 会获得完整的 ad-hoc 签名，本地打包与 CI 都会用严格的 `codesign` 校验阻止签名不完整的产物。ad-hoc 签名不等于 Apple 公证：浏览器下载的版本首次启动时仍可能需要用户在 macOS「隐私与安全性」中确认放行；要让任意 Mac 首次启动都不出现身份提示，必须使用 Developer ID Application 证书并完成公证。Windows Authenticode 仍是独立的可选加固，不会降低 Tauri 更新签名的强制要求。

## 运行环境变量

| 变量 | 含义 |
| --- | --- |
| `DSH_DESKTOP_HOME` | 启动器/运行环境目录，默认 `~/.dsh-desktop` |
| `DSH_HOME` | 显式外部 Harness 目录；会绕过桌面端隔离的 `dsh-home` 并关闭全部导入，只应有意设置 |
| `DSH_DESKTOP_SOURCE_HOME` | 可选的 source home，默认 `~/.dsh` |
| `DSH_DESKTOP_CC_SWITCH_HOME` | 可选的只读 CC Switch 来源，默认 `~/.cc-switch` |
| `DSH_DESKTOP_NODE_VERSION` | 精确 Node 覆盖值；必须同时设置 `DSH_DESKTOP_NODE_SHA256` |
| `DSH_DESKTOP_NODE_SHA256` | 自定义 Node 归档的 SHA-256 信任根 |
| `DSH_DESKTOP_NODE_BASES` | 逗号分隔的 Node 镜像；显式配置会关闭默认回退 |
| `DSH_DESKTOP_NPM_REGISTRIES` | 逗号分隔的 npm registry；显式配置会关闭默认回退 |

## 许可

启动器源码使用 MIT 许可。`@deepseek-ai/dsh`、Node.js、Tauri、React 及其他依赖继续适用各自的许可与条款。
