#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use codex_usage_monitor::app_server::run_query;
use codex_usage_monitor::error::AppError;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

struct MockCommand {
    _temp: TempDir,
    executable: PathBuf,
    codex_home: PathBuf,
    pid_file: PathBuf,
}

impl MockCommand {
    fn new(mode: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("mock-codex");
        let codex_home = temp.path().join("codex-home");
        let pid_file = temp.path().join("pid");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_codex.js");
        let script = format!(
            "#!/bin/sh\nMOCK_MODE={} MOCK_PID_FILE={} exec node {} \"$@\"\n",
            shell_quote(mode),
            shell_quote(&pid_file.to_string_lossy()),
            shell_quote(&fixture.to_string_lossy()),
        );
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            _temp: temp,
            executable,
            codex_home,
            pid_file,
        }
    }

    async fn run(
        &self,
        timeout: Duration,
    ) -> Result<codex_usage_monitor::account::CachedAccountData, AppError> {
        run_query(
            self.executable.clone(),
            self.codex_home.clone(),
            timeout,
            CancellationToken::new(),
        )
        .await
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[tokio::test(flavor = "current_thread")]
async fn mock_server_handles_multiple_windows_and_optional_usage() {
    let mock = MockCommand::new("success");
    let result = mock.run(Duration::from_secs(2)).await.unwrap();
    assert_eq!(
        result.identity.email.as_deref(),
        Some("fixture@example.com")
    );
    assert_eq!(result.quota.windows.len(), 2);
    assert_eq!(result.quota.windows[0].remaining_percent, 75.0);
    assert_eq!(
        result
            .quota
            .token_usage
            .as_ref()
            .and_then(|usage| usage.lifetime_tokens),
        Some(1234)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn mock_server_accepts_partial_stdout_messages() {
    let mock = MockCommand::new("partial");
    let result = mock.run(Duration::from_secs(2)).await.unwrap();
    assert_eq!(result.quota.windows.len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn mock_server_classifies_auth_timeout_protocol_and_exit_failures() {
    let unlogged = MockCommand::new("unlogged");
    assert!(matches!(
        unlogged.run(Duration::from_secs(2)).await,
        Err(AppError::NotLoggedIn)
    ));

    let timed_out = MockCommand::new("timeout");
    assert!(matches!(
        timed_out.run(Duration::from_millis(100)).await,
        Err(AppError::Timeout(_))
    ));

    let invalid = MockCommand::new("invalid_json");
    assert!(matches!(
        invalid.run(Duration::from_secs(2)).await,
        Err(AppError::InvalidResponse(_))
    ));

    let exited = MockCommand::new("early_exit");
    assert!(matches!(
        exited.run(Duration::from_secs(2)).await,
        Err(AppError::ProcessExited(_))
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn child_is_gone_after_query_completes() {
    let mock = MockCommand::new("success");
    mock.run(Duration::from_secs(2)).await.unwrap();
    let pid = fs::read_to_string(&mock.pid_file)
        .unwrap()
        .trim()
        .to_owned();
    tokio::time::sleep(Duration::from_millis(80)).await;
    let status = std::process::Command::new("/bin/kill")
        .args(["-0", &pid])
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "mock app-server process was left running"
    );
}
