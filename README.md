# Codex Usage Monitor

一个轻量、原生、跨平台的桌面工具，用来统一查看多个 ChatGPT 账号的
Codex 使用比例、剩余比例和重置时间。

它只调用本机 Codex CLI 的官方 App Server 账户接口，不发送模型对话，
不抓取网页，不读取浏览器 Cookie，不使用第三方额度服务，也不会在关闭窗口后
保留托盘或后台进程。

## 支持平台

- macOS 11 或更高版本（本项目实际在 Apple Silicon macOS 构建）
- Windows 10/11 x64（由 GitHub Actions 原生 Windows runner 构建）

界面基于 Rust stable、egui 0.35 和 eframe 0.35。macOS/Windows 使用同一套
业务与 UI 代码，支持 Retina/高 DPI、深浅色主题、中文字体和本地时区显示。
macOS 构建使用与系统一致的透明统一标题栏、原生交通灯按钮和可拖动标题栏区域；
Windows 保留标准窗口装饰。
设置中的“跟随系统 / 浅色 / 深色”会立即同步内容背景、控件和原生窗口装饰。

## 工作方式

每个本地账号对应一个随机 UUID 和一个独立的 `CODEX_HOME`。刷新时，应用为
该账号短暂启动：

```text
codex app-server --stdio
```

随后依次执行初始化、`account/read`、`account/rateLimits/read`，并在支持时
读取 `account/usage/read`。查询结束立即关闭 stdin 并等待子进程退出；超时才
强制终止这个由应用自己持有的进程。刷新全部默认依次处理账号，不会长期运行
多个 App Server。

详见 [架构说明](docs/ARCHITECTURE.md) 和
[协议兼容记录](docs/PROTOCOL_COMPATIBILITY.md)。

## 前置要求

运行时必须有支持以下稳定账户方法的 OpenAI Codex CLI：

- `account/read`
- `account/login/start` / `account/login/completed` / `account/login/cancel`
- `account/logout`
- `account/rateLimits/read`

应用启动时会显示实际路径、版本以及 `app-server` 能力。安装或更新 Codex
请以 [OpenAI Codex 官方文档](https://developers.openai.com/codex/) 为准。
如果 Codex 不在 GUI 应用可见的 `PATH` 中，可以在“设置”里填入实际可执行
文件路径。

开发构建还需要：

- Rust stable（项目当前以 Rust 1.97.1 验证，`rust-version` 为 1.92）
- macOS Command Line Tools，或 Windows MSVC Build Tools + Windows SDK

## 构建

通用检查：

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build --release
```

macOS 可双击应用和 ZIP：

```sh
./scripts/package-macos.sh
open "dist/Codex Usage Monitor.app"
```

产物为：

- `dist/Codex Usage Monitor.app`
- `dist/Codex-Usage-Monitor-macOS.zip`

Windows：

```powershell
cargo build --release
.\target\release\codex-usage-monitor.exe
```

Windows 构建设置了 GUI subsystem，因此双击不会额外弹出控制台窗口；
`build.rs` 还会写入应用名称、版本信息和图标。CI 会生成便携 ZIP。

## 首次启动

1. 双击应用。
2. 顶部确认显示“Codex 可用”，并检查路径和版本。
3. 点击“添加第一个账号”。
4. 输入只保存在本地的显示名称，例如“主账号”。
5. 选择“创建并用浏览器登录”。
6. 只在打开的 OpenAI 官方页面输入凭据。
7. 完成后应用会验证账号并立即读取额度。

如果浏览器回调失败，可取消后改用“设备码登录”。设备码和官方验证地址会显示
在应用窗口中。

## 添加第二和第三个账号

再次点击“添加账号”，分别创建“工作账号”“备用账号”等。每次都会创建全新、
相互隔离的状态目录。

浏览器通常会复用当前 ChatGPT 会话。添加第二、第三个账号时，请在 OpenAI
官方登录页退出或切换到正确账号，再授权 Codex。完整邮箱会显示在账号卡片并
保存在本机受限配置中，但不会作为内部 ID；账号内部身份仍是随机 UUID。

账号数量没有写死为三个，可以继续添加；“刷新全部”仍默认依次查询。

## 账号操作

- “刷新”只刷新当前账号。
- “浏览器重新登录”重新走官方 OAuth。
- “设备码登录”适合浏览器回调或默认浏览器异常时使用。
- “退出登录”调用官方 `account/logout`，不影响其他账号。
- “重命名”只修改本地显示名称。
- “启用”开关决定该账号是否参加自动/全部刷新。
- 查询或登录中可点击“取消”；应用会关闭对应 App Server。

## 删除账号与认证目录

点击“删除”后有两个明确选项：

- 不勾选：只从应用列表移除，账号目录和 Codex 凭据保留。
- 勾选：永久删除该账号受管的整个状态目录，包括官方 Codex 保存的凭据。

删除逻辑只接受应用数据目录下与账号 UUID 精确对应的路径，拒绝删除外部路径。
如果只删除了列表项，可稍后在应用数据目录的 `accounts` 子目录中手动删除遗留
目录。

## 本地数据位置

应用使用操作系统标准数据目录；“设置”和窗口底部会显示当前绝对路径。

典型位置：

- macOS：`~/Library/Application Support/com.NHNDeu.CodexUsageMonitor/`
- Windows：`%LOCALAPPDATA%\NHNDeu\CodexUsageMonitor\`

内容：

```text
config.json                  本地设置、完整邮箱、套餐和最后成功缓存（受限）
accounts/<uuid>/             该账号独立 CODEX_HOME（敏感）
accounts/<uuid>/auth.json    可能存在；由 Codex 官方管理（高度敏感）
logs/codex-usage-monitor.log 脱敏、限长诊断日志
```

配置带 `schema_version`，写入使用临时文件加原子替换。查询失败时保留最后成功
结果，并显示查询时间、缓存状态和过期状态。

## 安全说明

- 本应用没有密码输入框。不要把 ChatGPT 密码输入本应用。
- 不读取、解析、复制、上传或显示 `auth.json`。
- `account/read` 返回的完整邮箱会显示在账号卡片并保存在权限受限的本机
  `config.json`；不会上传到本项目的任何服务，也不会用作内部账号 ID。
- 不向诊断日志记录访问令牌、授权头、完整邮箱或完整授权响应。
- 启动 App Server 时会移除可能覆盖账号隔离的 API key/token 环境变量。
- 每个进程只收到自己的 `CODEX_HOME`，不会修改系统全局环境变量。
- 强制使用 Codex 官方 `file` 凭据存储以获得确定的多目录隔离。
- macOS/Unix 账号目录权限为 `0700`，配置文件为 `0600`。
- 日志约 1 MB 时轮转，写入前会脱敏；没有遥测或分析服务。
- 核心功能只让本地 Codex 与 OpenAI 官方服务通信。

绝对不要分享：

- 任意账号目录；
- `auth.json`；
- `.credentials.json`；
- 含访问令牌或完整授权 URL 的截图/日志。

## 应用访问与连接

应用直接访问自己的标准数据目录、你指定的 Codex 可执行文件以及系统字体。
登录时它要求操作系统打开 Codex 返回的 OpenAI HTTPS 地址。网络请求本身由
OpenAI 官方 Codex CLI 发往 OpenAI 服务；本项目没有开发者服务器、代理、
数据库或云同步。

## 常见问题

### 找不到 Codex

设置中填入实际文件路径并保存。应用会重新执行 `--version` 和
`app-server --help`。路径失效或能力缺失时不会伪造额度数据。

### macOS 从 Finder 启动时找不到终端里的 Codex

Finder 不会继承交互式 shell 的完整 `PATH`。应用额外检查：

- `/opt/homebrew/bin/codex`
- `/usr/local/bin/codex`
- `/Applications/ChatGPT.app/Contents/Resources/codex`
- `~/.local/bin/codex`
- `~/.npm-global/bin/codex`

仍未找到时，在设置中选择/填写绝对路径。

### Windows 找不到 Codex

优先在设置中填写实际的 `codex.exe`，不要填写只对某个终端有效的别名。
路径可以包含空格。应用也会检查 `PATH`、`LOCALAPPDATA`、`APPDATA` 和常见
程序目录。Windows 原生 Codex/安装布局可能随版本变化，手动路径是稳定后备。

### 未登录或登录失效

点击“浏览器重新登录”。不要把密码、访问令牌或 `auth.json` 发给开发者。

### 登录了错误账号

对该本地账号执行“退出登录”，然后重新登录；在 OpenAI 官方页面切换到正确的
ChatGPT 账号。其他本地账号不会受影响。

### OAuth 回调失败

取消当前登录并使用“设备码登录”。也可复制窗口中的官方地址到其他浏览器。

### 网络、DNS、TLS 或请求超时

历史成功数据仍会显示为缓存。展开“脱敏诊断信息”，确认网络恢复后再次刷新。
可以在设置中调整 5–120 秒请求超时。

### 协议不兼容

先升级 Codex。若最新版仍失败，按
[协议兼容记录](docs/PROTOCOL_COMPATIBILITY.md) 重新生成 schema，并只在
`protocol.rs`、`app_server.rs` 和 `rate_limits.rs` 更新适配。

### 配置损坏

应用会以空配置启动、显示错误，并保留原 `config.json`，不会用空数据覆盖损坏
文件。修复或备份后再重启。

## 未签名 macOS 应用

本地构建没有 Apple Developer 证书，因此没有签名或公证。首次打开下载的 ZIP
产物时，macOS 可能阻止运行。确认产物来自你信任的构建后，可在 Finder 中按住
Control 点击应用并选“打开”，或在“系统设置 → 隐私与安全性”中允许。

本项目不会声称本地构建已签名或公证。正式分发应在受控发布流程中加入
Developer ID 签名、公证和 stapling。

## 已知限制

- 没有真实 ChatGPT 登录时只能用模拟 App Server 验证协议、缓存和进程管理；
  完整在线验收必须由用户本人在 OpenAI 官方页面交互式登录。
- 当前机器不能原生验证 Windows GUI；Windows 编译、测试和 ZIP 由原生
  GitHub Actions runner 完成。
- 官方可能按账号/套餐只返回一个窗口。应用只展示实际返回的数据，不推算请求
  次数、Token 余额、分钟数或模型可运行次数。
- `account/usage/read` 是可选摘要；失败不会影响核心限额查询。
- 本项目没有安装器、后台刷新、托盘、通知、自动账号切换或任务分配。

## 测试

测试夹具模拟了完整 App Server JSONL 进程，覆盖：

- 已登录、未登录；
- 单/多窗口和未知字段；
- 无效百分比、缺失字段、无重置时间；
- 部分 stdout 消息、非法 JSON、超时、提前退出；
- 可选 usage；
- 多账号目录隔离、配置迁移、缓存状态；
- 查询完成后的子进程清理。

CI 在 macOS 和 Windows 分别执行格式检查、Clippy、测试和 release 构建，并上传
平台产物。

## 完全卸载

1. 在应用中逐个删除账号并勾选删除凭据，或关闭应用后删除整个应用数据目录。
2. 删除 `.app` 或 `.exe`/便携 ZIP。
3. 如需清除诊断日志，确认应用关闭后删除数据目录中的 `logs`。

应用不创建启动项、计划任务、系统服务、托盘进程或开发者服务器，因此没有其他
后台组件需要卸载。

## 许可证

[MIT](LICENSE)
