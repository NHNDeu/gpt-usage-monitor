use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};
use tokio_util::sync::CancellationToken;

use crate::account::{AccountIdentity, CachedAccountData, LoginChallenge, mask_email};
use crate::error::{AppError, Result};
use crate::protocol::{IncomingMessage, RpcError, notification, parse_line, request};
use crate::rate_limits::{parse_rate_limits, parse_token_usage};

const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(8);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(900);
const MAX_STDERR_LINES: usize = 24;

pub struct AppServerProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Lines<BufReader<ChildStdout>>,
    stderr_lines: Arc<Mutex<VecDeque<String>>>,
    stderr_task: JoinHandle<()>,
    pending_notifications: VecDeque<(String, Value)>,
    next_id: u64,
    request_timeout: Duration,
}

impl AppServerProcess {
    pub async fn spawn(
        codex_path: &Path,
        codex_home: &Path,
        request_timeout: Duration,
    ) -> Result<Self> {
        prepare_codex_home(codex_home)?;

        let mut command = Command::new(codex_path);
        command
            .arg("app-server")
            .arg("--stdio")
            .arg("-c")
            .arg("cli_auth_credentials_store=\"file\"")
            .arg("-c")
            .arg("analytics.enabled=false")
            .env("CODEX_HOME", codex_home)
            .env("RUST_LOG", "warn")
            .env("LOG_FORMAT", "json")
            .env_remove("OPENAI_API_KEY")
            .env_remove("CODEX_API_KEY")
            .env_remove("CODEX_ACCESS_TOKEN")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        configure_hidden_process(&mut command);

        let mut child = command.spawn().map_err(|error| {
            AppError::CodexUnavailable(format!("无法启动 {}：{error}", codex_path.display()))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::ProcessExited("无法连接 App Server 标准输入".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::ProcessExited("无法连接 App Server 标准输出".to_owned()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AppError::ProcessExited("无法连接 App Server 标准错误".to_owned()))?;

        let stderr_lines = Arc::new(Mutex::new(VecDeque::new()));
        let stderr_task = capture_stderr(stderr, Arc::clone(&stderr_lines));

        let mut server = Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout).lines(),
            stderr_lines,
            stderr_task,
            pending_notifications: VecDeque::new(),
            next_id: 1,
            request_timeout,
        };
        server.initialize().await?;
        Ok(server)
    }

    async fn initialize(&mut self) -> Result<()> {
        let token = CancellationToken::new();
        let params = json!({
            "clientInfo": {
                "name": "codex_usage_monitor",
                "title": "Codex Usage Monitor",
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": {
                "optOutNotificationMethods": [
                    "thread/started",
                    "turn/started",
                    "item/started",
                    "item/completed"
                ]
            }
        });
        self.request_with_timeout("initialize", Some(params), &token, INITIALIZE_TIMEOUT)
            .await?;
        self.send_value(&notification("initialized", None)).await
    }

    pub async fn query_account(&mut self, cancel: &CancellationToken) -> Result<CachedAccountData> {
        let account_result = self
            .request("account/read", Some(json!({"refreshToken": false})), cancel)
            .await?;
        let identity = parse_account_identity(&account_result)?;
        let rate_result = self
            .request("account/rateLimits/read", None, cancel)
            .await?;
        let mut quota = parse_rate_limits(&rate_result, Utc::now())?;

        // Usage is an optional secondary endpoint. Its absence must never hide valid quota data.
        if let Ok(usage) = self
            .request_with_timeout("account/usage/read", None, cancel, Duration::from_secs(5))
            .await
        {
            quota.token_usage = parse_token_usage(&usage);
        }

        Ok(CachedAccountData { identity, quota })
    }

    pub async fn start_login(
        &mut self,
        device_code: bool,
        cancel: &CancellationToken,
    ) -> Result<LoginChallenge> {
        let params = if device_code {
            json!({"type": "chatgptDeviceCode"})
        } else {
            json!({
                "type": "chatgpt",
                "useHostedLoginSuccessPage": true,
                "appBrand": "codex"
            })
        };
        let result = self
            .request("account/login/start", Some(params), cancel)
            .await?;

        let login_id = required_string(&result, "loginId")?;
        let (url, user_code) = if device_code {
            (
                required_string(&result, "verificationUrl")?,
                Some(required_string(&result, "userCode")?),
            )
        } else {
            (required_string(&result, "authUrl")?, None)
        };

        validate_official_login_url(&url)?;
        Ok(LoginChallenge {
            login_id,
            url,
            user_code,
            device_code,
        })
    }

    pub async fn wait_for_login(
        &mut self,
        challenge: &LoginChallenge,
        cancel: &CancellationToken,
        wait_timeout: Duration,
    ) -> Result<()> {
        if let Some(result) =
            take_login_completion(&mut self.pending_notifications, &challenge.login_id)
        {
            return result;
        }

        let deadline = Instant::now() + wait_timeout;
        loop {
            if Instant::now() >= deadline {
                return Err(AppError::Timeout("等待浏览器登录完成".to_owned()));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let message = self.next_message(cancel, remaining).await?;
            match message {
                IncomingMessage::Notification { method, params }
                    if method == "account/login/completed" =>
                {
                    if params.get("loginId").and_then(Value::as_str)
                        == Some(challenge.login_id.as_str())
                    {
                        if params
                            .get("success")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                        {
                            return Ok(());
                        }
                        let reason = params
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("OpenAI 登录未完成");
                        return Err(AppError::Server(crate::logging::redact(reason)));
                    }
                }
                IncomingMessage::Notification { method, params } => {
                    self.push_notification(method, params);
                }
                IncomingMessage::ServerRequest { id, method, .. } => {
                    self.reject_server_request(id, &method).await?;
                }
                IncomingMessage::Response { .. } => {}
            }
        }
    }

    pub async fn cancel_login(&mut self, login_id: &str) {
        let token = CancellationToken::new();
        let _ = self
            .request_with_timeout(
                "account/login/cancel",
                Some(json!({"loginId": login_id})),
                &token,
                Duration::from_secs(3),
            )
            .await;
    }

    pub async fn logout(&mut self, cancel: &CancellationToken) -> Result<()> {
        self.request("account/logout", None, cancel).await?;
        Ok(())
    }

    pub fn diagnostics(&self) -> String {
        let Ok(lines) = self.stderr_lines.lock() else {
            return String::new();
        };
        lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    pub async fn shutdown(mut self) {
        self.stdin.take();
        match timeout(SHUTDOWN_TIMEOUT, self.child.wait()).await {
            Ok(_) => {}
            Err(_) => {
                let _ = self.child.kill().await;
                let _ = self.child.wait().await;
            }
        }
        self.stderr_task.abort();
    }

    async fn request(
        &mut self,
        method: &str,
        params: Option<Value>,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        self.request_with_timeout(method, params, cancel, self.request_timeout)
            .await
    }

    async fn request_with_timeout(
        &mut self,
        method: &str,
        params: Option<Value>,
        cancel: &CancellationToken,
        request_timeout: Duration,
    ) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send_value(&request(id, method, params)).await?;

        let deadline = Instant::now() + request_timeout;
        loop {
            if Instant::now() >= deadline {
                return Err(AppError::Timeout(method.to_owned()));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.next_message(cancel, remaining).await? {
                IncomingMessage::Response {
                    id: response_id,
                    result,
                } if response_id == json!(id) => {
                    return result.map_err(|error| map_rpc_error(method, error));
                }
                IncomingMessage::Notification { method, params } => {
                    self.push_notification(method, params);
                }
                IncomingMessage::ServerRequest { id, method, .. } => {
                    self.reject_server_request(id, &method).await?;
                }
                IncomingMessage::Response { .. } => {
                    // A response for another request is unexpected in this sequential client.
                    // Ignore it rather than coupling UI state to a stale response.
                }
            }
        }
    }

    async fn next_message(
        &mut self,
        cancel: &CancellationToken,
        duration: Duration,
    ) -> Result<IncomingMessage> {
        tokio::select! {
            _ = cancel.cancelled() => Err(AppError::Cancelled),
            line = timeout(duration, self.stdout.next_line()) => {
                match line {
                    Err(_) => Err(AppError::Timeout("等待 App Server 响应".to_owned())),
                    Ok(Err(error)) => Err(AppError::Io(error)),
                    Ok(Ok(None)) => Err(AppError::ProcessExited(self.diagnostics())),
                    Ok(Ok(Some(line))) => parse_line(&line),
                }
            }
        }
    }

    async fn send_value(&mut self, value: &Value) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| AppError::ProcessExited("App Server 标准输入已经关闭".to_owned()))?;
        let mut encoded = serde_json::to_vec(value)
            .map_err(|error| AppError::InvalidResponse(error.to_string()))?;
        encoded.push(b'\n');
        stdin.write_all(&encoded).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn reject_server_request(&mut self, id: Value, method: &str) -> Result<()> {
        self.send_value(&json!({
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("Codex Usage Monitor does not handle server request {method}")
            }
        }))
        .await
    }

    fn push_notification(&mut self, method: String, params: Value) {
        if self.pending_notifications.len() >= 100 {
            self.pending_notifications.pop_front();
        }
        self.pending_notifications.push_back((method, params));
    }
}

fn capture_stderr(stderr: ChildStderr, lines: Arc<Mutex<VecDeque<String>>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let safe = crate::logging::redact(&line);
            crate::logging::warn(&safe);
            if let Ok(mut captured) = lines.lock() {
                if captured.len() >= MAX_STDERR_LINES {
                    captured.pop_front();
                }
                captured.push_back(safe);
            }
        }
    })
}

fn parse_account_identity(result: &Value) -> Result<AccountIdentity> {
    let Some(account) = result.get("account").filter(|account| !account.is_null()) else {
        return Err(AppError::NotLoggedIn);
    };
    if account.get("type").and_then(Value::as_str) != Some("chatgpt") {
        return Err(AppError::Server(
            "当前 Codex 登录不是 ChatGPT 订阅账号，无法查询 ChatGPT Codex 限额".to_owned(),
        ));
    }
    Ok(AccountIdentity {
        masked_email: account.get("email").and_then(Value::as_str).map(mask_email),
        plan_type: account
            .get("planType")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn required_string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| AppError::ProtocolIncompatible(format!("登录响应缺少字段 {field}")))
}

fn validate_official_login_url(raw: &str) -> Result<()> {
    let url = url::Url::parse(raw)
        .map_err(|error| AppError::InvalidResponse(format!("登录地址无效：{error}")))?;
    let allowed = matches!(
        url.host_str(),
        Some("chatgpt.com") | Some("auth.openai.com") | Some("openai.com")
    ) || url
        .host_str()
        .is_some_and(|host| host.ends_with(".openai.com"));
    if url.scheme() != "https" || !allowed {
        return Err(AppError::InvalidResponse(
            "Codex 返回了非 OpenAI 官方 HTTPS 登录地址".to_owned(),
        ));
    }
    Ok(())
}

fn take_login_completion(
    notifications: &mut VecDeque<(String, Value)>,
    login_id: &str,
) -> Option<Result<()>> {
    let position = notifications.iter().position(|(method, params)| {
        method == "account/login/completed"
            && params.get("loginId").and_then(Value::as_str) == Some(login_id)
    })?;
    let (_, params) = notifications.remove(position)?;
    Some(
        if params
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            Ok(())
        } else {
            Err(AppError::Server(
                params
                    .get("error")
                    .and_then(Value::as_str)
                    .map(crate::logging::redact)
                    .unwrap_or_else(|| "OpenAI 登录未完成".to_owned()),
            ))
        },
    )
}

fn map_rpc_error(method: &str, error: RpcError) -> AppError {
    let safe = crate::logging::redact(&error.message);
    let lower = safe.to_ascii_lowercase();
    if lower.contains("authentication required")
        || lower.contains("not logged")
        || lower.contains("unauthorized")
    {
        AppError::NotLoggedIn
    } else if error.code == Some(-32601) || lower.contains("method not found") {
        AppError::ProtocolIncompatible(format!("{method}：{safe}"))
    } else {
        AppError::Server(format!("{method}：{safe}"))
    }
}

fn prepare_codex_home(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    set_private_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn configure_hidden_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.as_std_mut().creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_hidden_process(_command: &mut Command) {}

pub async fn run_query(
    codex_path: PathBuf,
    codex_home: PathBuf,
    request_timeout: Duration,
    cancel: CancellationToken,
) -> Result<CachedAccountData> {
    let mut server = AppServerProcess::spawn(&codex_path, &codex_home, request_timeout).await?;
    let result = server.query_account(&cancel).await;
    let diagnostic = server.diagnostics();
    server.shutdown().await;
    result.map_err(|error| append_process_diagnostic(error, diagnostic))
}

fn append_process_diagnostic(error: AppError, diagnostic: String) -> AppError {
    if diagnostic.is_empty() {
        return error;
    }
    match error {
        AppError::ProcessExited(message) if message.is_empty() => {
            AppError::ProcessExited(diagnostic)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{parse_account_identity, validate_official_login_url};
    use crate::error::AppError;

    #[test]
    fn account_parser_accepts_unknown_fields_and_masks_email() {
        let result = parse_account_identity(&json!({
            "account": {
                "type": "chatgpt",
                "email": "someone@example.com",
                "planType": "plus",
                "future": true
            },
            "requiresOpenaiAuth": true
        }))
        .unwrap();
        assert_eq!(result.masked_email.as_deref(), Some("so***@example.com"));
    }

    #[test]
    fn account_parser_rejects_unlogged_and_api_key_accounts() {
        assert!(matches!(
            parse_account_identity(&json!({"account": null, "requiresOpenaiAuth": true})),
            Err(AppError::NotLoggedIn)
        ));
        assert!(parse_account_identity(&json!({"account": {"type": "apiKey"}})).is_err());
    }

    #[test]
    fn only_opens_official_https_login_urls() {
        assert!(validate_official_login_url("https://chatgpt.com/auth").is_ok());
        assert!(validate_official_login_url("https://auth.openai.com/codex/device").is_ok());
        assert!(validate_official_login_url("http://chatgpt.com/auth").is_err());
        assert!(validate_official_login_url("https://evil.example/openai").is_err());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn kill_on_drop_terminates_owned_child() {
        use std::process::Stdio;
        use tokio::process::Command;
        use tokio::time::{Duration, sleep};

        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "while true; do sleep 1; done"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let child = command.spawn().unwrap();
        let pid = child.id().unwrap();
        drop(child);
        sleep(Duration::from_millis(150)).await;

        let status = std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .status()
            .unwrap();
        assert!(!status.success(), "owned child process was not terminated");
    }
}
