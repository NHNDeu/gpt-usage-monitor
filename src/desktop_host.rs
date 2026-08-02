use std::path::PathBuf;

use crate::error::{AppError, Result};

#[derive(Debug, Clone)]
pub struct HostShutdown {
    pub was_running: bool,
    launch_target: Option<LaunchTarget>,
}

#[derive(Debug, Clone)]
enum LaunchTarget {
    #[cfg(target_os = "macos")]
    MacBundle(PathBuf),
    #[cfg(target_os = "windows")]
    WindowsExecutable(PathBuf),
    #[cfg(target_os = "windows")]
    WindowsAumid(String),
}

pub fn stop_for_switch() -> Result<HostShutdown> {
    platform::stop_for_switch()
}

pub fn restart_after_switch(shutdown: &HostShutdown) -> Result<bool> {
    if !shutdown.was_running {
        return Ok(false);
    }
    let Some(target) = &shutdown.launch_target else {
        return Err(AppError::DesktopSwitch(
            "凭据已切换，但找不到经过身份确认的桌面宿主启动目标，请手动打开 Codex 桌面应用"
                .to_owned(),
        ));
    };
    platform::restart(target)?;
    Ok(true)
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn is_macos_codex_bundle_id(bundle_id: &str) -> bool {
    bundle_id == "com.openai.codex"
}

#[cfg(any(target_os = "windows", test))]
pub(crate) fn is_windows_codex_host_path(
    path: &std::path::Path,
    embedded_codex_exists: bool,
) -> bool {
    let normalized = path.to_string_lossy().replace('\\', "/").to_lowercase();
    let name = normalized.rsplit('/').next().unwrap_or_default();
    if !matches!(name, "chatgpt.exe" | "codex.exe") {
        return false;
    }
    if normalized.contains("openai.chat")
        && !normalized.contains("openai.codex")
        && !embedded_codex_exists
    {
        return false;
    }
    normalized.contains("/windowsapps/openai.codex")
        || normalized.contains("/openai/codex/")
        || embedded_codex_exists
}

#[cfg(target_os = "macos")]
mod platform {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{HostShutdown, LaunchTarget, is_macos_codex_bundle_id};
    use crate::error::{AppError, Result};

    const GRACEFUL_TIMEOUT: Duration = Duration::from_secs(6);
    const TERM_TIMEOUT: Duration = Duration::from_secs(2);

    #[derive(Debug, Clone)]
    struct ProcessInfo {
        pid: u32,
        parent_pid: u32,
        executable: PathBuf,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum BundleKind {
        Codex,
        OrdinaryChat,
        EmbeddedCodexOnly,
        Other,
    }

    pub(super) fn stop_for_switch() -> Result<HostShutdown> {
        let processes = list_processes()?;
        let mut hosts = Vec::new();
        for process in &processes {
            let Some(bundle) = enclosing_app_bundle(&process.executable) else {
                if matches!(
                    process
                        .executable
                        .file_name()
                        .and_then(|name| name.to_str()),
                    Some("ChatGPT" | "Codex")
                ) {
                    return Err(AppError::DesktopSwitch(
                        "检测到无法定位所属 .app 的 ChatGPT/Codex 候选进程；无法确认身份，请手动关闭 Codex 桌面宿主后重试"
                            .to_owned(),
                    ));
                }
                continue;
            };
            match classify_bundle(&bundle) {
                BundleKind::Codex => hosts.push((process.clone(), bundle)),
                BundleKind::EmbeddedCodexOnly => {
                    return Err(AppError::DesktopSwitch(format!(
                        "检测到内含 Codex CLI 但 Bundle ID 不是 com.openai.codex 的运行中应用：{}。为避免误关闭，请手动退出后重试",
                        bundle.display()
                    )));
                }
                BundleKind::OrdinaryChat | BundleKind::Other => {}
            }
        }
        if hosts.is_empty() {
            return Ok(HostShutdown {
                was_running: false,
                launch_target: None,
            });
        }

        let unique_bundles: HashSet<_> = hosts.iter().map(|(_, bundle)| bundle.clone()).collect();
        if unique_bundles.len() > 1 {
            return Err(AppError::DesktopSwitch(
                "同时检测到多个 com.openai.codex 桌面宿主安装实例；请手动全部退出后重试".to_owned(),
            ));
        }

        let bundle = hosts[0].1.clone();
        let host_pids: HashSet<_> = hosts.iter().map(|(process, _)| process.pid).collect();
        let descendant_pids = descendant_pids(&processes, &host_pids);
        request_normal_quit();
        wait_until_absent(&host_pids, GRACEFUL_TIMEOUT)?;

        let mut remaining = verified_host_pids(&host_pids, &bundle)?;
        for pid in &remaining {
            signal(*pid, "-TERM")?;
        }
        if !remaining.is_empty() {
            wait_until_absent(&remaining.iter().copied().collect(), TERM_TIMEOUT)?;
            remaining = verified_host_pids(&remaining.iter().copied().collect(), &bundle)?;
            for pid in &remaining {
                signal(*pid, "-KILL")?;
            }
            wait_until_absent(&remaining.iter().copied().collect(), TERM_TIMEOUT)?;
            if !verified_host_pids(&remaining.iter().copied().collect(), &bundle)?.is_empty() {
                return Err(AppError::DesktopSwitch(
                    "已确认的 Codex 桌面宿主在有限等待后仍未退出".to_owned(),
                ));
            }
        }

        stop_captured_app_servers(&descendant_pids, &bundle)?;
        Ok(HostShutdown {
            was_running: true,
            launch_target: Some(LaunchTarget::MacBundle(bundle)),
        })
    }

    pub(super) fn restart(target: &LaunchTarget) -> Result<()> {
        let LaunchTarget::MacBundle(bundle) = target;
        if classify_bundle(bundle) != BundleKind::Codex {
            return Err(AppError::DesktopSwitch(
                "桌面宿主启动前的 Bundle ID 复核失败".to_owned(),
            ));
        }
        let status = Command::new("/usr/bin/open")
            .args(["-b", "com.openai.codex"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .status()?;
        if !status.success() {
            let fallback = Command::new("/usr/bin/open")
                .arg(bundle)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .status()?;
            if !fallback.success() {
                return Err(AppError::DesktopSwitch(
                    "凭据已切换，但重新启动 Codex 桌面宿主失败，请手动打开应用".to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn list_processes() -> Result<Vec<ProcessInfo>> {
        let output = Command::new("/bin/ps")
            .args(["-ww", "-axo", "pid=,ppid=,comm="])
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .output()?;
        if !output.status.success() {
            return Err(AppError::DesktopSwitch(
                "无法读取进程列表以确认桌面宿主身份".to_owned(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let mut parts = line.trim().splitn(3, char::is_whitespace);
                let pid = parts.next()?.parse().ok()?;
                let parent_pid = parts.next()?.trim().parse().ok()?;
                let executable = PathBuf::from(parts.next()?.trim());
                Some(ProcessInfo {
                    pid,
                    parent_pid,
                    executable,
                })
            })
            .collect())
    }

    fn enclosing_app_bundle(executable: &Path) -> Option<PathBuf> {
        executable.ancestors().find_map(|ancestor| {
            (ancestor
                .extension()
                .and_then(|extension| extension.to_str())
                == Some("app"))
            .then(|| ancestor.to_owned())
        })
    }

    fn classify_bundle(bundle: &Path) -> BundleKind {
        let bundle_id = read_bundle_id(bundle);
        if bundle_id.as_deref() == Some("com.openai.chat") {
            return BundleKind::OrdinaryChat;
        }
        if bundle_id.as_deref().is_some_and(is_macos_codex_bundle_id) {
            return BundleKind::Codex;
        }
        if bundle.join("Contents/Resources/codex").is_file() {
            BundleKind::EmbeddedCodexOnly
        } else {
            BundleKind::Other
        }
    }

    fn read_bundle_id(bundle: &Path) -> Option<String> {
        let output = Command::new("/usr/bin/plutil")
            .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-"])
            .arg(bundle.join("Contents/Info.plist"))
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn request_normal_quit() {
        let _ = Command::new("/usr/bin/osascript")
            .args(["-e", "tell application id \"com.openai.codex\" to quit"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    fn descendant_pids(processes: &[ProcessInfo], roots: &HashSet<u32>) -> HashSet<u32> {
        let mut descendants = HashSet::new();
        loop {
            let before = descendants.len();
            for process in processes {
                if roots.contains(&process.parent_pid) || descendants.contains(&process.parent_pid)
                {
                    descendants.insert(process.pid);
                }
            }
            if descendants.len() == before {
                return descendants;
            }
        }
    }

    fn verified_host_pids(expected: &HashSet<u32>, bundle: &Path) -> Result<Vec<u32>> {
        Ok(list_processes()?
            .into_iter()
            .filter(|process| expected.contains(&process.pid))
            .filter(|process| {
                enclosing_app_bundle(&process.executable).as_deref() == Some(bundle)
                    && classify_bundle(bundle) == BundleKind::Codex
            })
            .map(|process| process.pid)
            .collect())
    }

    fn wait_until_absent(pids: &HashSet<u32>, duration: Duration) -> Result<()> {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            let current: HashSet<_> = list_processes()?.into_iter().map(|item| item.pid).collect();
            if pids.is_disjoint(&current) {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(150));
        }
        Ok(())
    }

    fn signal(pid: u32, signal: &str) -> Result<()> {
        let status = Command::new("/bin/kill")
            .args([signal, &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .status()?;
        if !status.success() && list_processes()?.iter().any(|process| process.pid == pid) {
            Err(AppError::DesktopSwitch(format!(
                "无法结束已确认的 Codex 桌面宿主进程 PID {pid}"
            )))
        } else {
            Ok(())
        }
    }

    fn stop_captured_app_servers(pids: &HashSet<u32>, bundle: &Path) -> Result<()> {
        let mut signalled = HashSet::new();
        for process in list_processes()? {
            if !pids.contains(&process.pid) {
                continue;
            }
            let in_bundle = process.executable.starts_with(bundle);
            let is_codex = process
                .executable
                .file_name()
                .and_then(|name| name.to_str())
                == Some("codex");
            if in_bundle && is_codex {
                signal(process.pid, "-TERM")?;
                signalled.insert(process.pid);
            }
        }
        wait_until_absent(&signalled, TERM_TIMEOUT)?;
        for process in list_processes()? {
            if signalled.contains(&process.pid) {
                let in_bundle = process.executable.starts_with(bundle);
                let is_codex = process
                    .executable
                    .file_name()
                    .and_then(|name| name.to_str())
                    == Some("codex");
                if !in_bundle || !is_codex {
                    return Err(AppError::DesktopSwitch(format!(
                        "PID {} 的身份在结束 App Server 前发生变化",
                        process.pid
                    )));
                }
                signal(process.pid, "-KILL")?;
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use serde_json::Value;

    use super::{HostShutdown, LaunchTarget, is_windows_codex_host_path};
    use crate::error::{AppError, Result};

    #[derive(Debug, Clone)]
    struct ProcessInfo {
        pid: u32,
        parent_pid: u32,
        name: String,
        executable: PathBuf,
        command_line: String,
    }

    pub(super) fn stop_for_switch() -> Result<HostShutdown> {
        let processes = list_processes()?;
        if processes.iter().any(|process| {
            matches!(
                process.name.to_ascii_lowercase().as_str(),
                "chatgpt.exe" | "codex.exe"
            ) && process.executable.as_os_str().is_empty()
        }) {
            return Err(AppError::DesktopSwitch(
                "检测到无法读取完整路径的 ChatGPT/Codex 候选进程；无法确认身份，请手动关闭 Codex 桌面宿主后重试"
                    .to_owned(),
            ));
        }
        let hosts: Vec<_> = processes
            .iter()
            .filter(|process| is_confirmed_host(&process.executable))
            .cloned()
            .collect();
        if hosts.is_empty() {
            return Ok(HostShutdown {
                was_running: false,
                launch_target: None,
            });
        }
        let root_pids: HashSet<_> = hosts.iter().map(|process| process.pid).collect();
        let descendants = descendant_pids(&processes, &root_pids);
        request_normal_quit(&root_pids)?;
        wait_until_absent(&root_pids, Duration::from_secs(6))?;
        for pid in verified_host_pids(&root_pids)? {
            taskkill(pid)?;
        }
        wait_until_absent(&root_pids, Duration::from_secs(2))?;
        if !verified_host_pids(&root_pids)?.is_empty() {
            return Err(AppError::DesktopSwitch(
                "已确认的 Codex 桌面宿主在有限等待后仍未退出".to_owned(),
            ));
        }
        stop_captured_app_servers(&descendants, &hosts)?;

        let executable = hosts[0].executable.clone();
        let launch_target = if is_msix_path(&executable) {
            resolve_aumid(&executable).map(LaunchTarget::WindowsAumid)
        } else {
            Some(LaunchTarget::WindowsExecutable(executable))
        };
        Ok(HostShutdown {
            was_running: true,
            launch_target,
        })
    }

    pub(super) fn restart(target: &LaunchTarget) -> Result<()> {
        let mut command = match target {
            LaunchTarget::WindowsExecutable(path) => {
                if !is_confirmed_host(path) {
                    return Err(AppError::DesktopSwitch(
                        "桌面宿主启动前的路径身份复核失败".to_owned(),
                    ));
                }
                Command::new(path)
            }
            LaunchTarget::WindowsAumid(aumid) => {
                let mut command = Command::new("explorer.exe");
                command.arg(format!(r"shell:AppsFolder\{aumid}"));
                command
            }
        };
        configure_hidden(&mut command);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                AppError::DesktopSwitch(format!(
                    "凭据已切换，但重新启动 Codex 桌面宿主失败：{error}"
                ))
            })?;
        Ok(())
    }

    fn list_processes() -> Result<Vec<ProcessInfo>> {
        let script = "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,ExecutablePath,CommandLine | ConvertTo-Json -Compress";
        let output = powershell().args(["-Command", script]).output()?;
        if !output.status.success() {
            return Err(AppError::DesktopSwitch(
                "无法读取进程列表以确认桌面宿主身份".to_owned(),
            ));
        }
        let value: Value = serde_json::from_slice(&output.stdout)
            .map_err(|_| AppError::DesktopSwitch("Windows 进程列表格式无法解析".to_owned()))?;
        let entries = value.as_array().cloned().unwrap_or_else(|| vec![value]);
        Ok(entries
            .into_iter()
            .filter_map(|entry| {
                Some(ProcessInfo {
                    pid: u32::try_from(entry.get("ProcessId")?.as_u64()?).ok()?,
                    parent_pid: u32::try_from(entry.get("ParentProcessId")?.as_u64()?).ok()?,
                    name: entry
                        .get("Name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    executable: entry
                        .get("ExecutablePath")
                        .and_then(Value::as_str)
                        .map(PathBuf::from)
                        .unwrap_or_default(),
                    command_line: entry
                        .get("CommandLine")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                })
            })
            .collect())
    }

    fn is_confirmed_host(path: &std::path::Path) -> bool {
        let embedded = path.parent().is_some_and(|parent| {
            parent.join("resources/codex.exe").is_file()
                || parent.join("Resources/codex.exe").is_file()
        });
        is_windows_codex_host_path(path, embedded)
    }

    fn is_msix_path(path: &std::path::Path) -> bool {
        path.to_string_lossy()
            .replace('\\', "/")
            .to_lowercase()
            .contains("/windowsapps/")
    }

    fn request_normal_quit(pids: &HashSet<u32>) -> Result<()> {
        let ids = pids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let script = r#"Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class NativeClose {
  public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc callback, IntPtr extraData);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
}
'@
$targets = $env:CODEX_HOST_PIDS.Split(',')
[NativeClose]::EnumWindows({ param($h,$l) $pidValue=0; [NativeClose]::GetWindowThreadProcessId($h,[ref]$pidValue)|Out-Null; if($targets -contains [string]$pidValue){[NativeClose]::PostMessage($h,0x0010,[IntPtr]::Zero,[IntPtr]::Zero)|Out-Null}; return $true }, [IntPtr]::Zero) | Out-Null"#;
        let status = powershell()
            .env("CODEX_HOST_PIDS", ids)
            .args(["-Command", script])
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(AppError::DesktopSwitch(
                "无法请求 Codex 桌面宿主正常退出".to_owned(),
            ))
        }
    }

    fn verified_host_pids(expected: &HashSet<u32>) -> Result<Vec<u32>> {
        Ok(list_processes()?
            .into_iter()
            .filter(|process| {
                expected.contains(&process.pid) && is_confirmed_host(&process.executable)
            })
            .map(|process| process.pid)
            .collect())
    }

    fn taskkill(pid: u32) -> Result<()> {
        let mut command = Command::new("taskkill.exe");
        configure_hidden(&mut command);
        let status = command
            .args(["/PID", &pid.to_string(), "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .status()?;
        if !status.success() && list_processes()?.iter().any(|process| process.pid == pid) {
            Err(AppError::DesktopSwitch(format!(
                "无法结束已确认的 Codex 进程 PID {pid}"
            )))
        } else {
            Ok(())
        }
    }

    fn descendant_pids(processes: &[ProcessInfo], roots: &HashSet<u32>) -> HashSet<u32> {
        let mut descendants = HashSet::new();
        loop {
            let before = descendants.len();
            for process in processes {
                if roots.contains(&process.parent_pid) || descendants.contains(&process.parent_pid)
                {
                    descendants.insert(process.pid);
                }
            }
            if before == descendants.len() {
                return descendants;
            }
        }
    }

    fn stop_captured_app_servers(pids: &HashSet<u32>, hosts: &[ProcessInfo]) -> Result<()> {
        let host_dirs: Vec<_> = hosts
            .iter()
            .filter_map(|host| host.executable.parent().map(std::path::Path::to_owned))
            .collect();
        for process in list_processes()? {
            if !pids.contains(&process.pid)
                || !process.command_line.to_lowercase().contains("app-server")
            {
                continue;
            }
            let trusted = host_dirs
                .iter()
                .any(|directory| process.executable.starts_with(directory));
            if trusted {
                taskkill(process.pid)?;
            }
        }
        Ok(())
    }

    fn wait_until_absent(pids: &HashSet<u32>, duration: Duration) -> Result<()> {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            let current: HashSet<_> = list_processes()?.into_iter().map(|item| item.pid).collect();
            if pids.is_disjoint(&current) {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(150));
        }
        Ok(())
    }

    fn resolve_aumid(executable: &std::path::Path) -> Option<String> {
        let script = r#"$target=$env:CODEX_HOST_PATH; $package=Get-AppxPackage | Where-Object { $_.InstallLocation -and $target.StartsWith($_.InstallLocation,[System.StringComparison]::OrdinalIgnoreCase) } | Select-Object -First 1; if($package){$relative=$target.Substring($package.InstallLocation.Length).TrimStart('\'); $manifest=Get-AppxPackageManifest $package; $app=$manifest.Package.Applications.Application | Where-Object { $_.Executable -and $relative.Equals([string]$_.Executable,[System.StringComparison]::OrdinalIgnoreCase) } | Select-Object -First 1; if($app){Write-Output ($package.PackageFamilyName + '!' + $app.Id)}}"#;
        let output = powershell()
            .env("CODEX_HOST_PATH", executable)
            .args(["-Command", script])
            .output()
            .ok()?;
        let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        (output.status.success() && !value.is_empty()).then_some(value)
    }

    fn powershell() -> Command {
        let mut command = Command::new("powershell.exe");
        configure_hidden(&mut command);
        command.args(["-NoProfile", "-NonInteractive"]);
        command
    }

    fn configure_hidden(command: &mut Command) {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    use super::{HostShutdown, LaunchTarget};
    use crate::error::{AppError, Result};

    pub(super) fn stop_for_switch() -> Result<HostShutdown> {
        Err(AppError::DesktopSwitch(
            "当前平台不支持桌面账号切换".to_owned(),
        ))
    }

    pub(super) fn restart(_target: &LaunchTarget) -> Result<()> {
        Err(AppError::DesktopSwitch(
            "当前平台不支持桌面宿主启动".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        HostShutdown, is_macos_codex_bundle_id, is_windows_codex_host_path, restart_after_switch,
    };

    #[test]
    fn host_that_was_not_running_is_never_started() {
        let shutdown = HostShutdown {
            was_running: false,
            launch_target: None,
        };
        assert!(!restart_after_switch(&shutdown).unwrap());
    }

    #[test]
    fn macos_bundle_id_check_excludes_ordinary_chatgpt() {
        assert!(is_macos_codex_bundle_id("com.openai.codex"));
        assert!(!is_macos_codex_bundle_id("com.openai.chat"));
        assert!(!is_macos_codex_bundle_id("com.example.ChatGPT"));
    }

    #[test]
    fn windows_path_check_is_case_insensitive_and_needs_codex_evidence() {
        assert!(is_windows_codex_host_path(
            Path::new(r"C:\Program Files\WindowsApps\OpenAI.Codex_1.0\ChatGPT.exe"),
            false
        ));
        assert!(is_windows_codex_host_path(
            Path::new(r"C:\Users\Me\AppData\Local\Programs\ChatGPT\ChatGPT.exe"),
            true
        ));
        assert!(!is_windows_codex_host_path(
            Path::new(r"C:\Program Files\WindowsApps\OpenAI.Chat_1.0\ChatGPT.exe"),
            false
        ));
        assert!(is_windows_codex_host_path(
            Path::new(r"C:\Program Files\WindowsApps\OpenAI.ChatGPT_2.0\ChatGPT.exe"),
            true
        ));
        assert!(!is_windows_codex_host_path(
            Path::new(r"C:\Tools\ChatGPT.exe"),
            false
        ));
    }
}
