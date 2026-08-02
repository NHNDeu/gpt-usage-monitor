use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use eframe::egui;
use tokio::runtime::Builder;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::account::{AccountIdentity, AccountRecord, CachedAccountData, LoginChallenge};
use crate::account_switch::{
    CredentialOwner, DesktopCredentialInspection, harden_auth_file, inspect_global,
    replace_global_credentials, rollback_global_credentials, validate_target,
};
use crate::app_server::{AppServerProcess, run_identity_query, run_query};
use crate::codex_locator::{self, CodexInstallation};
use crate::desktop_host;
use crate::error::AppError;

#[derive(Debug, Clone, Copy)]
pub enum OperationKind {
    Query,
    Login,
    Logout,
    SwitchDesktopAccount,
    InspectDesktopAccount,
}

#[derive(Debug)]
pub enum WorkerEvent {
    CodexDetected(std::result::Result<CodexInstallation, WorkerFailure>),
    Started {
        account_id: Uuid,
        operation: OperationKind,
        step: &'static str,
    },
    Step {
        account_id: Uuid,
        step: &'static str,
    },
    LoginChallenge {
        account_id: Uuid,
        challenge: LoginChallenge,
    },
    BrowserOpenFailed {
        account_id: Uuid,
        detail: String,
    },
    QueryFinished {
        account_id: Uuid,
        data: CachedAccountData,
    },
    LoginFinished {
        account_id: Uuid,
        data: CachedAccountData,
    },
    LogoutFinished {
        account_id: Uuid,
    },
    DesktopInspected {
        inspection: DesktopCredentialInspection,
        verified_identity: Option<AccountIdentity>,
    },
    DesktopSwitchFinished {
        account_id: Uuid,
        identity: AccountIdentity,
        recovery_path: Option<PathBuf>,
        already_active: bool,
        warning: Option<String>,
    },
    Failed {
        account_id: Uuid,
        operation: OperationKind,
        failure: WorkerFailure,
    },
}

#[derive(Debug)]
pub struct WorkerFailure {
    pub summary: String,
    pub diagnostic: String,
    pub category: FailureCategory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCategory {
    Cancelled,
    NotLoggedIn,
    TimedOut,
    CodexUnavailable,
    ProtocolIncompatible,
    Other,
}

impl From<AppError> for WorkerFailure {
    fn from(error: AppError) -> Self {
        let category = match error {
            AppError::Cancelled => FailureCategory::Cancelled,
            AppError::NotLoggedIn => FailureCategory::NotLoggedIn,
            AppError::Timeout(_) => FailureCategory::TimedOut,
            AppError::CodexUnavailable(_) => FailureCategory::CodexUnavailable,
            AppError::ProtocolIncompatible(_) => FailureCategory::ProtocolIncompatible,
            _ => FailureCategory::Other,
        };
        Self {
            summary: error.user_message().to_owned(),
            diagnostic: error.diagnostic(),
            category,
        }
    }
}

pub struct WorkerManager {
    sender: Sender<WorkerEvent>,
    receiver: Receiver<WorkerEvent>,
    active: HashMap<Uuid, CancellationToken>,
    threads: Vec<JoinHandle<()>>,
    codex_detection_active: bool,
    desktop_operation: Option<DesktopOperation>,
    desktop_cancel: Option<CancellationToken>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DesktopOperation {
    Inspect,
    Switch(Uuid),
}

impl Default for WorkerManager {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkerManager {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            sender,
            receiver,
            active: HashMap::new(),
            threads: Vec::new(),
            codex_detection_active: false,
            desktop_operation: None,
            desktop_cancel: None,
        }
    }

    pub fn detect_codex(&mut self, custom_path: Option<PathBuf>, ctx: egui::Context) {
        if self.codex_detection_active {
            return;
        }
        self.codex_detection_active = true;
        let sender = self.sender.clone();
        self.threads.push(thread::spawn(move || {
            let result = runtime()
                .map_err(AppError::Io)
                .and_then(|runtime| runtime.block_on(codex_locator::locate(custom_path)))
                .map_err(WorkerFailure::from);
            let _ = sender.send(WorkerEvent::CodexDetected(result));
            ctx.request_repaint();
        }));
    }

    pub fn refresh_one(
        &mut self,
        account: AccountRecord,
        codex_path: PathBuf,
        request_timeout: Duration,
        ctx: egui::Context,
    ) {
        if self.any_active() {
            return;
        }
        let cancel = CancellationToken::new();
        self.active.insert(account.id, cancel.clone());
        let sender = self.sender.clone();
        self.threads.push(thread::spawn(move || {
            send(
                &sender,
                &ctx,
                WorkerEvent::Started {
                    account_id: account.id,
                    operation: OperationKind::Query,
                    step: "↻ 正在查询",
                },
            );
            let result = runtime().map_err(AppError::Io).and_then(|runtime| {
                runtime.block_on(run_query(
                    codex_path,
                    account.state_dir,
                    request_timeout,
                    cancel,
                ))
            });
            match result {
                Ok(data) => send(
                    &sender,
                    &ctx,
                    WorkerEvent::QueryFinished {
                        account_id: account.id,
                        data,
                    },
                ),
                Err(error) => send_failure(&sender, &ctx, account.id, OperationKind::Query, error),
            }
        }));
    }

    pub fn refresh_all(
        &mut self,
        accounts: Vec<AccountRecord>,
        codex_path: PathBuf,
        request_timeout: Duration,
        ctx: egui::Context,
    ) {
        if self.any_active() {
            return;
        }
        let accounts: Vec<_> = accounts
            .into_iter()
            .filter(|account| account.enabled && !self.active.contains_key(&account.id))
            .collect();
        if accounts.is_empty() {
            return;
        }

        let mut jobs = Vec::with_capacity(accounts.len());
        for account in accounts {
            let cancel = CancellationToken::new();
            self.active.insert(account.id, cancel.clone());
            jobs.push((account, cancel));
        }

        let sender = self.sender.clone();
        self.threads.push(thread::spawn(move || {
            let runtime = match runtime() {
                Ok(runtime) => runtime,
                Err(error) => {
                    let error_kind = error.kind();
                    let error_message = error.to_string();
                    for (account, _) in jobs {
                        send_failure(
                            &sender,
                            &ctx,
                            account.id,
                            OperationKind::Query,
                            AppError::Io(std::io::Error::new(error_kind, error_message.clone())),
                        );
                    }
                    return;
                }
            };

            for (account, cancel) in jobs {
                if cancel.is_cancelled() {
                    send_failure(
                        &sender,
                        &ctx,
                        account.id,
                        OperationKind::Query,
                        AppError::Cancelled,
                    );
                    continue;
                }
                send(
                    &sender,
                    &ctx,
                    WorkerEvent::Started {
                        account_id: account.id,
                        operation: OperationKind::Query,
                        step: "↻ 正在查询",
                    },
                );
                let result = runtime.block_on(run_query(
                    codex_path.clone(),
                    account.state_dir,
                    request_timeout,
                    cancel,
                ));
                match result {
                    Ok(data) => send(
                        &sender,
                        &ctx,
                        WorkerEvent::QueryFinished {
                            account_id: account.id,
                            data,
                        },
                    ),
                    Err(error) => {
                        send_failure(&sender, &ctx, account.id, OperationKind::Query, error)
                    }
                }
            }
        }));
    }

    pub fn login(
        &mut self,
        account: AccountRecord,
        codex_path: PathBuf,
        request_timeout: Duration,
        device_code: bool,
        ctx: egui::Context,
    ) {
        if self.any_active() {
            return;
        }
        let cancel = CancellationToken::new();
        self.active.insert(account.id, cancel.clone());
        let sender = self.sender.clone();
        self.threads.push(thread::spawn(move || {
            send(
                &sender,
                &ctx,
                WorkerEvent::Started {
                    account_id: account.id,
                    operation: OperationKind::Login,
                    step: "↻ 正在启动官方登录",
                },
            );
            let result = runtime().map_err(AppError::Io).and_then(|runtime| {
                runtime.block_on(async {
                    let mut server =
                        AppServerProcess::spawn(&codex_path, &account.state_dir, request_timeout)
                            .await?;
                    let result = async {
                        let challenge = server.start_login(device_code, &cancel).await?;
                        send(
                            &sender,
                            &ctx,
                            WorkerEvent::LoginChallenge {
                                account_id: account.id,
                                challenge: challenge.clone(),
                            },
                        );
                        if let Err(error) = open::that(&challenge.url) {
                            send(
                                &sender,
                                &ctx,
                                WorkerEvent::BrowserOpenFailed {
                                    account_id: account.id,
                                    detail: error.to_string(),
                                },
                            );
                        }
                        send(
                            &sender,
                            &ctx,
                            WorkerEvent::Step {
                                account_id: account.id,
                                step: "↻ 等待 OpenAI 登录完成",
                            },
                        );
                        if let Err(error) = server
                            .wait_for_login(&challenge, &cancel, Duration::from_secs(600))
                            .await
                        {
                            if matches!(error, AppError::Cancelled) {
                                server.cancel_login(&challenge.login_id).await;
                            }
                            return Err(error);
                        }
                        send(
                            &sender,
                            &ctx,
                            WorkerEvent::Step {
                                account_id: account.id,
                                step: "↻ 正在读取额度",
                            },
                        );
                        server.query_account(&cancel).await
                    }
                    .await;
                    server.shutdown().await;
                    result
                })
            });
            match result {
                Ok(data) => send(
                    &sender,
                    &ctx,
                    WorkerEvent::LoginFinished {
                        account_id: account.id,
                        data,
                    },
                ),
                Err(error) => send_failure(&sender, &ctx, account.id, OperationKind::Login, error),
            }
        }));
    }

    pub fn logout(
        &mut self,
        account: AccountRecord,
        codex_path: PathBuf,
        request_timeout: Duration,
        ctx: egui::Context,
    ) {
        if self.any_active() {
            return;
        }
        let cancel = CancellationToken::new();
        self.active.insert(account.id, cancel.clone());
        let sender = self.sender.clone();
        self.threads.push(thread::spawn(move || {
            send(
                &sender,
                &ctx,
                WorkerEvent::Started {
                    account_id: account.id,
                    operation: OperationKind::Logout,
                    step: "↻ 正在退出登录",
                },
            );
            let result = runtime().map_err(AppError::Io).and_then(|runtime| {
                runtime.block_on(async {
                    let mut server =
                        AppServerProcess::spawn(&codex_path, &account.state_dir, request_timeout)
                            .await?;
                    let result = server.logout(&cancel).await;
                    server.shutdown().await;
                    result
                })
            });
            match result {
                Ok(()) => send(
                    &sender,
                    &ctx,
                    WorkerEvent::LogoutFinished {
                        account_id: account.id,
                    },
                ),
                Err(error) => send_failure(&sender, &ctx, account.id, OperationKind::Logout, error),
            }
        }));
    }

    pub fn inspect_desktop_account(
        &mut self,
        accounts: Vec<AccountRecord>,
        codex_path: PathBuf,
        global_home: PathBuf,
        request_timeout: Duration,
        ctx: egui::Context,
    ) -> bool {
        let Some(cancel) = self.reserve_desktop_operation(DesktopOperation::Inspect) else {
            return false;
        };
        let sender = self.sender.clone();
        self.threads.push(thread::spawn(move || {
            let result = runtime().map_err(AppError::Io).and_then(|runtime| {
                let mut inspection = inspect_global(&global_home, &accounts)?;
                let verified_identity = if inspection.identity.is_some() {
                    let mut verified = runtime.block_on(run_identity_query(
                        codex_path,
                        global_home,
                        request_timeout,
                        cancel,
                    ))?;
                    if !inspection
                        .identity
                        .as_ref()
                        .is_some_and(|identity| identity.matches_account(&verified))
                    {
                        inspection.owner = CredentialOwner::Unmanaged;
                    }
                    if verified.account_id.is_none() {
                        verified.account_id = inspection
                            .identity
                            .as_ref()
                            .and_then(|identity| identity.account_id.clone());
                    }
                    Some(verified)
                } else {
                    None
                };
                Ok((inspection, verified_identity))
            });
            match result {
                Ok((inspection, verified_identity)) => send(
                    &sender,
                    &ctx,
                    WorkerEvent::DesktopInspected {
                        inspection,
                        verified_identity,
                    },
                ),
                Err(error) => send_failure(
                    &sender,
                    &ctx,
                    Uuid::nil(),
                    OperationKind::InspectDesktopAccount,
                    error,
                ),
            }
        }));
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub fn switch_desktop_account(
        &mut self,
        target: AccountRecord,
        accounts: Vec<AccountRecord>,
        codex_path: PathBuf,
        global_home: PathBuf,
        recovery_root: PathBuf,
        request_timeout: Duration,
        ctx: egui::Context,
    ) -> bool {
        let Some(cancel) = self.reserve_desktop_operation(DesktopOperation::Switch(target.id))
        else {
            return false;
        };
        let sender = self.sender.clone();
        self.threads.push(thread::spawn(move || {
            send(
                &sender,
                &ctx,
                WorkerEvent::Started {
                    account_id: target.id,
                    operation: OperationKind::SwitchDesktopAccount,
                    step: "↻ 正在检查目标账号",
                },
            );
            let result = runtime().map_err(AppError::Io).and_then(|runtime| {
                let expected = validate_target(&target)?;
                let mut target_identity = runtime.block_on(run_identity_query(
                    codex_path.clone(),
                    target.state_dir.clone(),
                    request_timeout,
                    cancel.clone(),
                ))?;
                if !expected.matches_account(&target_identity) {
                    return Err(AppError::DesktopSwitch(
                        "目标 auth.json 身份与目标账号 account/read 返回不一致".to_owned(),
                    ));
                }
                if target_identity.account_id.is_none() {
                    target_identity.account_id.clone_from(&expected.account_id);
                }

                let current = inspect_global(&global_home, &accounts)?;
                if current.owner == CredentialOwner::Managed(target.id)
                    && let Ok(global_identity) = runtime.block_on(run_identity_query(
                        codex_path.clone(),
                        global_home.clone(),
                        request_timeout,
                        cancel.clone(),
                    ))
                    && expected.matches_account(&global_identity)
                {
                    harden_auth_file(&global_home)?;
                    return Ok((target_identity, None, true, None));
                }

                if cancel.is_cancelled() {
                    return Err(AppError::Cancelled);
                }
                send(
                    &sender,
                    &ctx,
                    WorkerEvent::Step {
                        account_id: target.id,
                        step: "↻ 正在识别并关闭桌面应用",
                    },
                );
                // From this point onward the transaction is intentionally non-cancellable.
                let host = desktop_host::stop_for_switch()?;
                send(
                    &sender,
                    &ctx,
                    WorkerEvent::Step {
                        account_id: target.id,
                        step: "↻ 正在保存当前账号",
                    },
                );
                send(
                    &sender,
                    &ctx,
                    WorkerEvent::Step {
                        account_id: target.id,
                        step: "↻ 正在切换凭据",
                    },
                );
                let receipt = match replace_global_credentials(
                    &target,
                    &accounts,
                    &global_home,
                    &recovery_root,
                ) {
                    Ok(receipt) => receipt,
                    Err(error) => {
                        let _ = desktop_host::restart_after_switch(&host);
                        return Err(error);
                    }
                };
                let mut warnings = Vec::new();
                if matches!(
                    receipt.previous_owner,
                    CredentialOwner::Unmanaged | CredentialOwner::Ambiguous
                ) {
                    warnings.push(
                        "切换前的全局凭据属于未管理或身份有歧义的账号，已保存受限恢复副本"
                            .to_owned(),
                    );
                }
                if let Err(error) = desktop_host::restart_after_switch(&host) {
                    warnings.push(error.diagnostic());
                }
                let warning = (!warnings.is_empty()).then(|| warnings.join("；"));
                if host.was_running {
                    send(
                        &sender,
                        &ctx,
                        WorkerEvent::Step {
                            account_id: target.id,
                            step: "↻ 正在重新启动应用",
                        },
                    );
                }
                send(
                    &sender,
                    &ctx,
                    WorkerEvent::Step {
                        account_id: target.id,
                        step: "↻ 正在验证桌面账号",
                    },
                );
                let verification = runtime.block_on(run_identity_query(
                    codex_path,
                    global_home,
                    request_timeout,
                    CancellationToken::new(),
                ));
                match verification {
                    Ok(mut identity) if expected.matches_account(&identity) => {
                        if identity.account_id.is_none() {
                            identity.account_id.clone_from(&expected.account_id);
                        }
                        Ok((identity, receipt.recovery_path.clone(), false, warning))
                    }
                    Ok(_) | Err(_) => {
                        let running_after_restart = desktop_host::stop_for_switch();
                        let shutdown = match running_after_restart {
                            Ok(shutdown) => shutdown,
                            Err(error) => {
                                let recovery = receipt
                                    .recovery_path
                                    .as_ref()
                                    .map(|path| path.display().to_string())
                                    .unwrap_or_else(|| {
                                        "切换前不存在全局 auth.json".to_owned()
                                    });
                                return Err(AppError::DesktopSwitch(format!(
                                    "切换后身份校验失败，且无法安全关闭已重新启动的桌面宿主，因此未自动回滚。恢复信息：{recovery}。{}",
                                    error.diagnostic()
                                )));
                            }
                        };
                        let rollback = rollback_global_credentials(&receipt);
                        let _ = desktop_host::restart_after_switch(&shutdown);
                        let recovery = receipt
                            .recovery_path
                            .as_ref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "切换前不存在全局 auth.json".to_owned());
                        match rollback {
                            Ok(()) => Err(AppError::DesktopSwitch(format!(
                                "切换后身份校验失败，已回滚全局凭据。恢复信息：{recovery}"
                            ))),
                            Err(error) => Err(AppError::DesktopSwitch(format!(
                                "切换后身份校验失败且自动回滚失败；请从受限恢复副本手动恢复：{recovery}。{}",
                                error.diagnostic()
                            ))),
                        }
                    }
                }
            });
            match result {
                Ok((identity, recovery_path, already_active, warning)) => send(
                    &sender,
                    &ctx,
                    WorkerEvent::DesktopSwitchFinished {
                        account_id: target.id,
                        identity,
                        recovery_path,
                        already_active,
                        warning,
                    },
                ),
                Err(error) => send_failure(
                    &sender,
                    &ctx,
                    target.id,
                    OperationKind::SwitchDesktopAccount,
                    error,
                ),
            }
        }));
        true
    }

    pub fn cancel(&self, account_id: Uuid) {
        if let Some(token) = self.active.get(&account_id) {
            token.cancel();
        }
    }

    pub fn is_active(&self, account_id: Uuid) -> bool {
        self.active.contains_key(&account_id)
    }

    pub fn any_active(&self) -> bool {
        !self.active.is_empty() || self.desktop_operation.is_some()
    }

    pub fn desktop_switch_active(&self) -> bool {
        matches!(self.desktop_operation, Some(DesktopOperation::Switch(_)))
    }

    pub fn cancel_all(&self) {
        for token in self.active.values() {
            token.cancel();
        }
        if let Some(token) = &self.desktop_cancel {
            token.cancel();
        }
    }

    pub fn drain_events(&mut self) -> Vec<WorkerEvent> {
        let events: Vec<_> = self.receiver.try_iter().collect();
        for event in &events {
            match event {
                WorkerEvent::CodexDetected(_) => self.codex_detection_active = false,
                WorkerEvent::QueryFinished { account_id, .. }
                | WorkerEvent::LoginFinished { account_id, .. }
                | WorkerEvent::LogoutFinished { account_id }
                | WorkerEvent::Failed { account_id, .. } => {
                    self.active.remove(account_id);
                }
                WorkerEvent::DesktopInspected { .. }
                | WorkerEvent::DesktopSwitchFinished { .. } => {
                    self.desktop_operation = None;
                    self.desktop_cancel = None;
                }
                _ => {}
            }
            if matches!(
                event,
                WorkerEvent::Failed {
                    operation: OperationKind::SwitchDesktopAccount
                        | OperationKind::InspectDesktopAccount,
                    ..
                }
            ) {
                self.desktop_operation = None;
                self.desktop_cancel = None;
            }
        }
        self.threads.retain(|thread| !thread.is_finished());
        events
    }

    pub fn shutdown(&mut self) {
        for token in self.active.values() {
            token.cancel();
        }
        if let Some(token) = &self.desktop_cancel {
            token.cancel();
        }
        while let Some(thread) = self.threads.pop() {
            let _ = thread.join();
        }
        self.active.clear();
        self.desktop_operation = None;
        self.desktop_cancel = None;
    }

    fn reserve_desktop_operation(
        &mut self,
        operation: DesktopOperation,
    ) -> Option<CancellationToken> {
        if self.any_active() {
            return None;
        }
        let cancel = CancellationToken::new();
        self.desktop_operation = Some(operation);
        self.desktop_cancel = Some(cancel.clone());
        Some(cancel)
    }
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    Builder::new_current_thread().enable_all().build()
}

fn send(sender: &Sender<WorkerEvent>, ctx: &egui::Context, event: WorkerEvent) {
    let _ = sender.send(event);
    ctx.request_repaint();
}

fn send_failure(
    sender: &Sender<WorkerEvent>,
    ctx: &egui::Context,
    account_id: Uuid,
    operation: OperationKind,
    error: AppError,
) {
    crate::logging::warn(format!(
        "账号 {account_id} 操作失败：{}",
        error.diagnostic()
    ));
    send(
        sender,
        ctx,
        WorkerEvent::Failed {
            account_id,
            operation,
            failure: error.into(),
        },
    );
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{DesktopOperation, WorkerManager};

    #[test]
    fn second_desktop_switch_is_rejected_by_global_lock() {
        let mut worker = WorkerManager::new();
        assert!(
            worker
                .reserve_desktop_operation(DesktopOperation::Switch(Uuid::new_v4()))
                .is_some()
        );
        assert!(
            worker
                .reserve_desktop_operation(DesktopOperation::Switch(Uuid::new_v4()))
                .is_none()
        );
    }
}
