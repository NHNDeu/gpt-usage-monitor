# 桌面账号切换

## 适用对象

本功能只切换承载 Codex 的桌面宿主：macOS 当前通常显示为 `ChatGPT.app`、
Bundle ID 为 `com.openai.codex`，历史版本可能显示为 `Codex.app`；Windows
宿主可能显示为 `ChatGPT.exe` 或 `Codex.exe`。普通聊天版 ChatGPT 不是操作
目标。

功能默认关闭。用户必须在“设置 → 桌面账号切换”中主动启用。切换会中断宿主中
正在运行的 Codex 任务，建议先结束任务。

## 事务流程

1. 确认目标 `accounts/<uuid>/` 是普通目录，`auth.json` 是普通文件且属于
   ChatGPT OAuth 文件型凭据。
2. 临时启动目标账号的官方 App Server，以 `account/read` 验证目标身份。
3. 读取全局 Codex Home（默认用户目录下 `.codex`），从全局 `auth.json` 提取
   必要身份字段，并与每个受管目录的实际凭据逐一匹配。
4. 安全识别桌面宿主。若运行中，先请求正常退出；超时后只对重新核验身份的 PID
   发送终止信号。同时清理由该宿主派生且已提前捕获的 App Server 子进程。
5. 为现有全局凭据创建受限恢复副本。只有稳定账号 ID 或唯一、经过验证的完整
   邮箱匹配成功，才把全局凭据保存回对应受管账号目录。
6. 在全局 `auth.json` 同目录创建随机临时文件，写入、flush、`sync_all`、设置
   权限，再用原子替换提交。
7. 仅当宿主在切换前正在运行时才重新启动；原本未运行时不会擅自启动。
8. 以全局 Codex Home 再次调用 `account/read`。身份一致才显示成功；身份不符或
   无法验证时会先关闭已重启宿主并自动回滚。若无法安全关闭宿主，应用不会在其
   运行中改回文件，而会明确保留恢复路径供人工处理。

## 防串号规则

匹配优先级固定为：

```text
稳定 ChatGPT account_id
  > ID Token 中标记为已验证的完整邮箱
  > 无法可靠匹配
```

显示名称、账号卡片 UUID、上次选择和 `last_active_desktop_account_id` 都不参与
凭据归属判断。稳定 ID 或邮箱命中多个受管目录时视为歧义，不写回任何受管目录。
目标文件、全局文件或某个受管文件损坏时，不会用损坏身份推断账号。

应用只解析 `auth_mode`、账号 ID、ID Token 的已验证邮箱等身份字段。不会显示、
记录或上传 access token、refresh token、完整 ID Token、授权 URL 或原始 JSON。

## 平台差异

### macOS

- 优先核验 `.app/Contents/Info.plist` 的 Bundle ID `com.openai.codex`；
- 明确排除 `com.openai.chat`；
- `Contents/Resources/codex` 只作为候选证据。若应用内含 Codex CLI 但 Bundle ID
  不是预期值，不会强制结束，用户需要手动关闭；
- 正常退出使用 Bundle ID；超时后按 PID 重新定位 `.app` 并再次核验 Bundle ID；
- 重启优先使用 `open -b com.openai.codex`，失败时才使用已核验应用路径。

### Windows

- 进程名只是候选入口，不能单独构成身份；
- 同时检查完整可执行路径、`OpenAI.Codex*` MSIX/WindowsApps 身份或安装目录中的
  `resources/codex.exe`；
- 明确排除只能证明属于普通 `OpenAI.Chat*` 的路径；
- 先向经过确认的窗口 PID 发送正常关闭消息，必要时再按重新核验的 PID 强制结束；
- MSIX 从 manifest 解析 AppsFolder 标识，普通安装使用已核验的可执行文件路径。
  无法可靠解析启动目标时，凭据仍可完成切换和验证，但会提示手动打开应用。

不能使用 `killall ChatGPT` 或 `taskkill /IM ChatGPT.exe`：两个不同产品可能共享
显示名称，按名称结束会误伤普通聊天客户端或其他用户进程。

## 恢复目录与手动恢复

恢复副本位于应用数据目录：

```text
desktop-auth-recovery/<UTC时间>-<随机UUID>/auth.json
```

如果当前全局凭据匹配某个受管账号，目录中还可能包含该受管账号被更新前的
`managed-<uuid>-previous.json`。目录权限在 Unix/macOS 为 `0700`，凭据文件为
`0600`。恢复副本不会自动清理，因为自动清理可能删除用户唯一的未知账号凭据。

手动恢复时：

1. 完全退出经过身份确认的 Codex 桌面宿主和相关 App Server；
2. 备份当前全局 `.codex/auth.json`；
3. 将所需恢复副本在 `.codex` 同目录写入临时文件，设置受限权限并原子改名为
   `auth.json`；
4. 手动打开 Codex 桌面宿主并用 `account/read` 或本应用顶部状态重新验证。

不要把恢复目录、凭据或包含令牌的终端输出发送给他人。

## 已知限制

- 只支持文件型凭据。显式配置为 macOS Keychain、Windows 凭据管理器或其他
  keyring 时会拒绝切换；
- ChatGPT Web Cookie 和嵌入 Web 页面会话不保证随 `auth.json` 同步；
- 未来桌面应用的 Bundle ID、Package Family、路径、进程树或凭据格式可能变化；
  检测无法建立可靠身份时会保守失败；
- 本功能不保存或恢复正在运行的 Codex 任务，不提供后台、定时或自动轮换。

关闭功能只需在设置中取消勾选并保存。关闭不会删除账号凭据、全局凭据或已有恢复
副本。删除账号时，“只删除列表”和“同时删除凭据目录”仍是两个独立选择。
