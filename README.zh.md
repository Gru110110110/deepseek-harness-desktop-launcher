# DSH Launcher

[English](README.md) | 中文

一个面向普通用户、为 DeepSeek Harness 提供桌面启动能力的非官方项目：双击打开后自动下载并启动 DeepSeek Harness 服务，随后可用已安装的浏览器打开服务发布的 Web UI 地址。关闭桌面窗口只会隐藏到系统托盘，本地服务继续运行；托盘中的「退出」才会完整停止服务。启动器界面在 macOS 和 Windows 上支持简体中文与英文，打包由 GitHub Actions 完成。

DSH Launcher 会在运行时下载上游 `@deepseek-ai/dsh` 软件包。该软件包仍受其自身许可证和条款约束；本仓库的 MIT 许可证仅覆盖 DSH Launcher 自身代码，不会对 `@deepseek-ai/dsh`、DeepSeek Harness 或 DeepSeek 的商标及品牌素材重新授权。

## 工作原理

启动器本身只是一个「壳」。桌面版本会固定一份经过验证的 Node 归档；首次安装时从已配置的注册表解析当前 Harness 版本，并将结果冻结为这次部署的精确目标。每次启动都会验证已安装的 manifest 与可执行入口；在线检查只提示语义版本严格更高的更新：

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

首次安装时，启动器会并发查询已配置注册表中的有效 `latest` 元数据，选择最高的 SemVer，并将该结果冻结用于本次部署。Node 归档通过有时间限制、支持断点续传的传输下载，并且必须匹配发布版本中的 SHA-256；npm 会从首先证明所选精确 Harness 版本可达的注册表安装。Node 传输在重试时会在官方端点与 npmmirror 之间交替，npm 则会并发探测两个注册表；任何显式源列表都是权威配置。Node 传输显示根据字节数计算的百分比；元数据查询、解压、npm 安装、验证、切换与服务启动没有可信总量，因此显示精确活动和持续更新的已用时间，而不伪造百分比。点击「立即更新」会在 staging 中安装已经选定的精确候选版本，验证后原子切换目录并重启服务；失败时恢复之前的目录和版本标记。运行时数据目录：

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

服务数据默认隔离在 `~/.dsh-desktop/dsh-home`。`.source-home-import-v1` 不存在时，或者该 home 没有任何配置时，启动器会从 `~/.dsh` 导入缺失配置项；已有文件优先，已有目录只补缺失的后代条目。只有目标端既没有 `sessions` 也没有 `attachments` 时，才会把这两个目录作为一组历史数据复制。独立的 `.source-workspace-import-v1` 快照会从 `storages/workspace.json` 导入兼容的 workspace v2 分组账本：目标文件缺失时直接接收；经过验证、已初始化且没有工作区和归档会话的空账本可以由源码账本修复；桌面端非空或无法识别的账本始终优先。两个完成标记都会阻止后续启动继续同步源码端新增数据。迁移不包含其他 `storages` 文件、匿名用户标识、已安装的 `node_modules`、临时写入文件和符号链接。

上述快照完成后，启动器只会作出一次可选的 CC Switch 判定；无论数据库不存在、无法安全读取、被跳过还是成功导入，都会写入 `.cc-switch-import-v2`。v2 用保守的“仅补缺失项”合并替代了早期“整份文档存在就保留”的判定，因此旧 v1 标记不会阻止这次修正后的一次性流程。这唯一一次检查会以 SQLite 只读模式打开 `~/.cc-switch/cc-switch.db`。只转换 CC Switch 中可独立工作的 Claude Code 供应商：必须具有非回环 HTTP(S) 端点、字面 API Key、至少一个模型，并使用 DSH 支持的 Anthropic Messages、OpenAI Chat Completions 或 OpenAI Responses 协议。OAuth／托管账号、`PROXY_MANAGED`、本地路由端点、完整 URL 覆盖、不支持的格式和字段不完整记录均会跳过。兼容的既有 JSON 文档只深度补入缺失路由与凭据引用；保守识别的 YAML mapping 可以追加缺失的 `llm-pi-ai` 分节和新凭据键，而不重写原有文本。任何既有值始终优先，无法识别的结构或冲突路由会原样保留并跳过。候选 `settings.yaml` 与 `.credentials.yaml` 会先在 staging 中生成并验证，再以仅限所有者的权限原子发布；部分发布会恢复两个文件的原始字节，密钥不会进入 settings 或日志，之后每次启动都会直接跳过 CC Switch，不再打开其数据库。显式 `DSH_HOME` 会跳过全部导入。该一次性判定会追加到 `server.log`，但不记录配置内容。

`language` 没有有效的已保存选择时，启动器读取操作系统有序的界面语言偏好，选择第一个已提供的主语言（`zh` 或 `en`）；两者都不存在时回退为中文。侧栏语言菜单立即应用选择，并以原子方式保存显式选择。此启动器偏好与 Harness home 中保存的 Web UI 语言相互独立。

**安装失败排查**：npm 安装会写入 `~/.dsh-desktop/install.log`。启动器会并发探测已配置的注册表，优先尝试可达源，并限制完整安装区间。失败尝试绝不会删除活动运行时；日志记录每个源的 npm 输出，Node 传输与校验和失败会直接显示在启动器中。关闭窗口后部署进程会在托盘继续运行；使用托盘「退出」才会取消并等待该进程结束。

## 目录结构

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
│   ├── windows.spec         # Windows onedir 打包配置（不再自解压）
│   ├── package_windows.ps1  # 生成 Windows 发布 ZIP
│   ├── sign_windows.ps1     # CI 中可选的 Authenticode 签名
│   ├── create_macos_dmg.sh  # 制作 DMG
│   └── generate_icons.py    # 生成已签入的应用图标（维护用）
├── tests/                   # 非 GUI 模块的单元测试
├── assets/                  # 应用图标
└── build-local.sh / .bat    # 本地打包脚本
```

## 本地构建

需要带 Tk 的 Python 3.11。构建脚本会在 `.build-venv` 中安装固定版本的托盘与打包依赖。

macOS（生成 `.app` 与 DMG）：

```sh
# brew install python-tk@3.11   # 如缺 tkinter
./build-local.sh
```

Windows（生成 `DSHLauncher-Windows-x64.zip`）：

```bat
build-local.bat
```

产物在 `dist/`。运行时（Node + dsh）在首次运行时自动下载安装，本地构建无需额外准备。

## GitHub Actions 打包

工作流：`.github/workflows/desktop.yml`。

- **手动触发**：在 Actions 页选择 *Build Desktop App* → *Run workflow*，产物以 artifacts 形式上传（不含 Release）。
- **自动发版**：推送 `desktop-v*` 标签（如 `desktop-v0.1.1`）时，运行 desktop 测试、通过官方源与 npmmirror 验证固定的 Node 元数据及有效的 Harness `latest` 元数据、构建 macOS (arm64/x64) 与 Windows (x64) 安装包、通过 `--check` 冒烟测试每个打包启动器、生成 `SHA256SUMS.txt`，并创建 GitHub Release。

## 发版流程

1. 更新 `app_paths.py` 中的 `APP_VERSION`，它决定 `desktop-v<APP_VERSION>` Release 标签。变更 `NODE_VERSION` 时，必须在同一变更中根据已签名的 Node 发布 manifest 更新 `runtime.py` 中所有支持平台的归档哈希。任一默认 Node manifest 与固定归档不一致，或任一默认 npm 注册表未返回有效的 Harness `latest` 元数据时，打包都会失败。
2. 打标签并推送，触发构建与发版：

   ```sh
   git tag desktop-v0.1.1
   git push origin desktop-v0.1.1
   ```

3. 从 Release 下载对应平台的安装包（`.dmg` / Windows `.zip`）及 `SHA256SUMS.txt`，一并分发给用户。

## 测试

```sh
python3.11 -m unittest discover -s tests -v
```

## 环境变量（可选）

| 变量 | 作用 |
| --- | --- |
| `DSH_DESKTOP_HOME` | 覆盖数据目录（默认 `~/.dsh-desktop`） |
| `DSH_DESKTOP_SOURCE_HOME` | 覆盖可选的一次性导入来源（默认 `~/.dsh`）；测试和构建检查应使用隔离路径 |
| `DSH_DESKTOP_CC_SWITCH_HOME` | 覆盖可选的只读 CC Switch 来源目录（默认 `~/.cc-switch`）；CC Switch 使用自定义数据目录及隔离测试时必须设置 |
| `DSH_DESKTOP_NODE_BASES` | 逗号分隔的 Node 发行版源；显式列表会禁用公共默认值 |
| `DSH_DESKTOP_NODE_BASE` | 未设置复数形式时使用的单一权威 Node 发行版源 |
| `DSH_DESKTOP_NODE_VERSION` | 覆盖固定的精确 Node 版本；必须同时设置 `DSH_DESKTOP_NODE_SHA256` |
| `DSH_DESKTOP_NODE_SHA256` | 覆盖 Node 归档时使用的可信 SHA-256 |
| `DSH_DESKTOP_NPM_REGISTRIES` | 逗号分隔的 npm 注册表；显式列表会禁用公共默认值 |
| `DSH_DESKTOP_NPM_REGISTRY` | 未设置复数形式时使用的单一权威 npm 注册表 |
| `DSH_DESKTOP_NETWORK_TIMEOUT_SECONDS` | 单次 HTTP 操作的连接与空闲读取超时（默认 10） |
| `DSH_DESKTOP_DOWNLOAD_TIMEOUT_SECONDS` | Node 下载跨重试与多源的总区间（默认 600） |
| `DSH_DESKTOP_INSTALL_TIMEOUT_SECONDS` | npm 安装跨注册表的总区间（默认 900） |

## 注意事项

- **Windows 杀毒兼容性**：Windows 改为 PyInstaller 单目录应用，并以 `DSHLauncher-Windows-x64.zip` 分发，去掉单文件 bootloader 运行时自解压这一常见启发式误报源。使用时要先完整解压 `DSHLauncher` 文件夹，再运行其中的 `DSHLauncher.exe`；不要把 EXE 单独移出 `_internal` 目录。
- **Windows 可选签名**：没有证书时仍可正常构建和发布。若以后需要 Authenticode，在 GitHub 仓库中配置 `WINDOWS_SIGNING_CERT_BASE64`（PFX 的 Base64 内容）与 `WINDOWS_SIGNING_CERT_PASSWORD` 两个 Secrets；工作流会使用 SHA-256 与 RFC 3161 时间戳签名，并在生成 ZIP 前验证签名。可用仓库变量 `WINDOWS_TIMESTAMP_URL` 覆盖默认时间戳服务。
- **macOS 签名**：macOS 保持 ad-hoc 签名且不阻止发布，首次打开可能仍需右键 →「打开」或按系统提示放行。Developer ID 签名和公证属于可选的后续增强，不是构建前提；macOS 本来就不是 Windows 的单文件 bootloader，因此无需套用此次 Windows 打包改造。
- **macOS 架构构建**：Apple Silicon 安装包使用 `macos-15` 运行器，Intel 安装包使用 `macos-15-intel`。CI 与本地构建脚本都会在创建或上传 DMG 前通过 `lipo` 校验启动器的实际 Mach-O 架构。
- **运行时部署**：Node/npm 官方端点与 npmmirror 是默认传输源，但只有 SHA-256 和精确版本决定哪些内容可以进入安装。Node 部分下载与 npm 内容缓存可跨重试保留。跨进程锁会串行化写入方；staging 目录、可执行 smoke、原子版本标记、保留的前一目录和启动恢复共同防止中断或失败更新替换最后一份有效运行时。npm 子进程会接收代理与证书设置，但不会接收环境中的 API key、密码、token 或用户 npm 配置。
- **地址发现**：启动器首先使用官方默认地址。仅当服务报告 `EADDRINUSE` 时，才用官方 `--port 0` 参数重试，让操作系统选择空闲的回环端口。官方 `dsh web: <URL>` 输出始终是就绪信号，以及唯一显示和打开的地址。若服务因其他原因退出或 60 秒内没有发布地址，启动器会停止服务并报告失败，绝不使用桌面端自行设定的 host 或端口作为回退。
- **浏览器选择**：启动器会在启动时检测常见的已安装浏览器。仅检测到一款时只显示打开按钮；检测到多款时在按钮旁提供浏览器菜单。若未识别到浏览器，则仅保留系统默认浏览器作为回退。点击 Logo 会在所选浏览器中打开 `https://dsdesktop.com`，服务发布的 Web UI 地址也可点击复制。指定浏览器会把每个 URL 作为独立进程参数接收，不经过 shell。
- **系统托盘生命周期**：托盘图标在启动器整个生命周期内保持可见。关闭窗口只隐藏窗口；「显示启动主页面」会恢复窗口，「打开Web UI」使用当前选择的浏览器且在服务发布地址前保持禁用，「退出」会取消部署、停止完整服务进程树并结束启动器。macOS 的 Command-Q 遵循系统退出语义。若托盘启动失败，窗口不会隐藏到不可恢复状态，关闭窗口会回退为完整退出。
- **启动器语言**：有效的已保存 `language` 值优先于操作系统偏好。没有该值时，macOS 读取 `AppleLanguages`，Windows 读取用户首选界面语言，其他系统读取标准 locale 环境值。切换语言无需重启服务即可更新当前启动器状态，包括延迟渲染的错误；第三方原始错误详情保持不变。
- **数据隔离与首次迁移**：桌面端使用 `~/.dsh-desktop/dsh-home`。版本化标记让源码配置／历史、工作区分组和兼容的 CC Switch Claude 供应商分别成为一次性快照。缺失的源码 home 配置后代可以补入已有桌面目录，但绝不替换任何路径；只有桌面端同时缺少 `sessions` 和 `attachments` 时才复制源码历史。唯一导入的存储域文件是兼容的 workspace v2 账本，且只有目标文件缺失或经过验证为空时才会接收；其他可变存储、依赖安装目录和符号链接仍不迁移。CC Switch 数据库只读打开，只通过保守的“仅补缺失项”合并贡献可独立工作的供应商字段；既有值、OAuth 状态与本地路由状态不会导入或覆盖。迁移时，桌面端 Harness 版本必须能够读取导入的磁盘格式；完成快照后，各套 home 可以继续分离。若迁移后的 profile 依赖外部插件，需要在桌面端 home 中另行安装对应依赖。`server.log` 会记录每类迁移结果和原因，但不含密钥。
