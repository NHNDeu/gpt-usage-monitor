use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use eframe::egui;
use tokio::runtime::Builder;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::account::{AccountRecord, CachedAccountData, LoginChallenge};
use crate::app_server::{AppServerProcess, run_query};
use crate::codex_locator::{self, CodexInstallation};
use crate::error::AppError;

#[derive(Debug, Clone, Copy)]
pub enum OperationKind {
    Query,
    Login,
    Logout,
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
        if self.active.contains_key(&account.id) {
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
        if self.active.contains_key(&account.id) {
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
        if self.active.contains_key(&account.id) {
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

    pub fn cancel(&self, account_id: Uuid) {
        if let Some(token) = self.active.get(&account_id) {
            token.cancel();
        }
    }

    pub fn is_active(&self, account_id: Uuid) -> bool {
        self.active.contains_key(&account_id)
    }

    pub fn any_active(&self) -> bool {
        !self.active.is_empty()
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
                _ => {}
            }
        }
        self.threads.retain(|thread| !thread.is_finished());
        events
    }

    pub fn shutdown(&mut self) {
        for token in self.active.values() {
            token.cancel();
        }
        while let Some(thread) = self.threads.pop() {
            let _ = thread.join();
        }
        self.active.clear();
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
