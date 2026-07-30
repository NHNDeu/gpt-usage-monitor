use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::time::timeout;

use crate::error::{AppError, Result};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CodexInstallation {
    pub path: PathBuf,
    pub version: String,
    pub supports_app_server: bool,
}

pub async fn locate(custom_path: Option<PathBuf>) -> Result<CodexInstallation> {
    if let Some(path) = custom_path
        .as_ref()
        .filter(|path| !path.as_os_str().is_empty())
    {
        if !path.is_file() {
            return Err(AppError::CodexUnavailable(format!(
                "自定义路径不存在或不是文件：{}",
                path.display()
            )));
        }
        return probe(path).await;
    }
    let candidates = candidate_paths(None);
    let mut failures = Vec::new();

    for path in candidates {
        match probe(&path).await {
            Ok(installation) => return Ok(installation),
            Err(error) => failures.push(format!("{}: {}", path.display(), error.diagnostic())),
        }
    }

    let detail = if failures.is_empty() {
        "PATH 和常见安装位置中均未找到 codex".to_owned()
    } else {
        failures.join("\n")
    };
    Err(AppError::CodexUnavailable(detail))
}

pub async fn probe(path: &Path) -> Result<CodexInstallation> {
    let version_output = run_capture(path, &["--version"]).await?;
    if !version_output.status.success() {
        return Err(AppError::CodexUnavailable(format!(
            "{} --version 退出码 {:?}",
            path.display(),
            version_output.status.code()
        )));
    }
    let version = String::from_utf8_lossy(&version_output.stdout)
        .trim()
        .to_owned();
    if version.is_empty() {
        return Err(AppError::CodexUnavailable(
            "版本命令没有返回版本号".to_owned(),
        ));
    }

    let help_output = run_capture(path, &["app-server", "--help"]).await?;
    let help = String::from_utf8_lossy(&help_output.stdout);
    let supports_app_server = help_output.status.success() && help.contains("app server");
    if !supports_app_server {
        return Err(AppError::ProtocolIncompatible(format!(
            "已检测到 {version}，但不支持 codex app-server"
        )));
    }

    Ok(CodexInstallation {
        path: path.to_owned(),
        version,
        supports_app_server,
    })
}

fn candidate_paths(custom_path: Option<PathBuf>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = custom_path.filter(|path| !path.as_os_str().is_empty()) {
        candidates.push(path);
    }

    if let Some(paths) = env::var_os("PATH") {
        for directory in env::split_paths(&paths) {
            candidates.push(directory.join(executable_name()));
        }
    }

    #[cfg(target_os = "macos")]
    {
        candidates.extend([
            PathBuf::from("/opt/homebrew/bin/codex"),
            PathBuf::from("/usr/local/bin/codex"),
            PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex"),
        ]);
        if let Some(home) = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_owned()) {
            candidates.push(home.join(".local/bin/codex"));
            candidates.push(home.join(".npm-global/bin/codex"));
            candidates.push(home.join(".cargo/bin/codex"));
        }
    }

    #[cfg(target_os = "windows")]
    {
        for variable in ["LOCALAPPDATA", "APPDATA", "ProgramFiles"] {
            if let Some(base) = env::var_os(variable).map(PathBuf::from) {
                candidates.push(base.join("OpenAI").join("Codex").join("codex.exe"));
                candidates.push(base.join("npm").join("codex.exe"));
                candidates.push(base.join("Programs").join("Codex").join("codex.exe"));
            }
        }
    }

    let mut seen = HashSet::<OsString>::new();
    candidates
        .into_iter()
        .filter(|path| path.is_file())
        .filter(|path| seen.insert(path.as_os_str().to_os_string()))
        .collect()
}

#[cfg(target_os = "windows")]
fn executable_name() -> &'static str {
    "codex.exe"
}

#[cfg(not(target_os = "windows"))]
fn executable_name() -> &'static str {
    "codex"
}

async fn run_capture(path: &Path, args: &[&str]) -> Result<std::process::Output> {
    let mut command = Command::new(path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_hidden_process(&mut command);
    let child = command.spawn().map_err(|error| {
        AppError::CodexUnavailable(format!("无法执行 {}：{error}", path.display()))
    })?;
    timeout(PROBE_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| AppError::Timeout(format!("检测 {}", path.display())))?
        .map_err(AppError::Io)
}

#[cfg(windows)]
fn configure_hidden_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.as_std_mut().creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_hidden_process(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::candidate_paths;

    #[test]
    fn explicit_nonexistent_path_is_not_reported_as_candidate() {
        let paths = candidate_paths(Some(PathBuf::from(
            "/definitely/not/a/real/codex-executable",
        )));
        assert!(
            !paths
                .iter()
                .any(|path| path == &PathBuf::from("/definitely/not/a/real/codex-executable"))
        );
    }
}
