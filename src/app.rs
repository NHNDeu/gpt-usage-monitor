use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use eframe::egui;
use uuid::Uuid;

use crate::account::{AccountRuntime, AccountStatus};
use crate::codex_locator::CodexInstallation;
use crate::error::AppError;
use crate::storage::{AppConfig, Storage, ThemePreference};
use crate::worker::{FailureCategory, WorkerEvent, WorkerManager};

#[derive(Debug)]
pub enum CodexState {
    Detecting,
    Available(CodexInstallation),
    Unavailable { summary: String, diagnostic: String },
}

#[derive(Debug, Default)]
pub struct AddDialog {
    pub name: String,
}

#[derive(Debug)]
pub struct EditDialog {
    pub account_id: Uuid,
    pub name: String,
}

#[derive(Debug)]
pub struct DeleteDialog {
    pub account_id: Uuid,
    pub delete_credentials: bool,
}

pub struct MonitorApp {
    pub(crate) storage: Option<Storage>,
    pub(crate) config: AppConfig,
    pub(crate) runtimes: HashMap<Uuid, AccountRuntime>,
    pub(crate) worker: WorkerManager,
    pub(crate) codex_state: CodexState,
    pub(crate) last_global_refresh: Option<DateTime<Utc>>,
    pub(crate) global_message: Option<String>,
    pub(crate) global_diagnostic: Option<String>,
    pub(crate) add_dialog: Option<AddDialog>,
    pub(crate) edit_dialog: Option<EditDialog>,
    pub(crate) delete_dialog: Option<DeleteDialog>,
    pub(crate) show_settings: bool,
    pub(crate) settings_path_buffer: String,
    auto_refresh_pending: bool,
}

impl MonitorApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (storage, config, startup_message) = match Storage::discover() {
            Ok(storage) => {
                crate::logging::init(storage.logs_root());
                let (config, warning) = storage.load_or_default();
                (Some(storage), config, warning)
            }
            Err(error) => (
                None,
                AppConfig::default(),
                Some(format!(
                    "无法使用标准应用数据目录，本次不会保存配置。\n{}",
                    error.diagnostic()
                )),
            ),
        };

        crate::platform::install_fonts(&cc.egui_ctx);
        let settings_path_buffer = config
            .settings
            .custom_codex_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let runtimes = config
            .accounts
            .iter()
            .map(|account| (account.id, AccountRuntime::default()))
            .collect();
        let auto_refresh_pending =
            config.settings.auto_refresh_on_start && !config.accounts.is_empty();

        let mut app = Self {
            storage,
            config,
            runtimes,
            worker: WorkerManager::new(),
            codex_state: CodexState::Detecting,
            last_global_refresh: None,
            global_message: startup_message,
            global_diagnostic: None,
            add_dialog: None,
            edit_dialog: None,
            delete_dialog: None,
            show_settings: false,
            settings_path_buffer,
            auto_refresh_pending,
        };
        app.apply_theme(&cc.egui_ctx);
        app.start_codex_detection(cc.egui_ctx.clone());
        app
    }

    pub(crate) fn process_events(&mut self, ctx: &egui::Context) {
        for event in self.worker.drain_events() {
            match event {
                WorkerEvent::CodexDetected(Ok(installation)) => {
                    crate::logging::info(format!(
                        "检测到 Codex {} ({})",
                        installation.version,
                        installation.path.display()
                    ));
                    self.codex_state = CodexState::Available(installation);
                    self.global_diagnostic = None;
                }
                WorkerEvent::CodexDetected(Err(failure)) => {
                    self.codex_state = CodexState::Unavailable {
                        summary: failure.summary,
                        diagnostic: failure.diagnostic,
                    };
                    self.auto_refresh_pending = false;
                }
                WorkerEvent::Started {
                    account_id,
                    operation,
                    step,
                } => {
                    let _ = operation;
                    let runtime = self.runtimes.entry(account_id).or_default();
                    runtime.status = AccountStatus::Querying(step);
                    runtime.error_summary = None;
                    runtime.diagnostic = None;
                }
                WorkerEvent::Step { account_id, step } => {
                    self.runtimes.entry(account_id).or_default().status =
                        AccountStatus::Querying(step);
                }
                WorkerEvent::LoginChallenge {
                    account_id,
                    challenge,
                } => {
                    self.runtimes.entry(account_id).or_default().login_challenge = Some(challenge);
                }
                WorkerEvent::BrowserOpenFailed { account_id, detail } => {
                    let runtime = self.runtimes.entry(account_id).or_default();
                    runtime.error_summary =
                        Some("无法自动打开浏览器，请点击登录窗口中的链接".to_owned());
                    runtime.diagnostic = Some(crate::logging::redact(&detail));
                }
                WorkerEvent::QueryFinished { account_id, data }
                | WorkerEvent::LoginFinished { account_id, data } => {
                    if let Some(account) = self
                        .config
                        .accounts
                        .iter_mut()
                        .find(|account| account.id == account_id)
                    {
                        account.last_attempt_at = Some(Utc::now());
                        account.last_success = Some(data);
                    }
                    let runtime = self.runtimes.entry(account_id).or_default();
                    runtime.status = AccountStatus::Success;
                    runtime.error_summary = None;
                    runtime.diagnostic = None;
                    runtime.login_challenge = None;
                    self.last_global_refresh = Some(Utc::now());
                    self.save_config();
                }
                WorkerEvent::LogoutFinished { account_id } => {
                    if let Some(account) = self
                        .config
                        .accounts
                        .iter_mut()
                        .find(|account| account.id == account_id)
                    {
                        account.last_attempt_at = Some(Utc::now());
                    }
                    let runtime = self.runtimes.entry(account_id).or_default();
                    runtime.status = AccountStatus::NotLoggedIn;
                    runtime.login_challenge = None;
                    runtime.error_summary = None;
                    runtime.diagnostic = None;
                    self.save_config();
                }
                WorkerEvent::Failed {
                    account_id,
                    operation,
                    failure,
                } => {
                    let _ = operation;
                    if let Some(account) = self
                        .config
                        .accounts
                        .iter_mut()
                        .find(|account| account.id == account_id)
                    {
                        account.last_attempt_at = Some(Utc::now());
                    }
                    let runtime = self.runtimes.entry(account_id).or_default();
                    runtime.status = match failure.category {
                        FailureCategory::Cancelled => AccountStatus::Idle,
                        FailureCategory::NotLoggedIn => AccountStatus::NotLoggedIn,
                        FailureCategory::TimedOut => AccountStatus::TimedOut,
                        FailureCategory::CodexUnavailable => AccountStatus::CodexUnavailable,
                        FailureCategory::ProtocolIncompatible => {
                            AccountStatus::ProtocolIncompatible
                        }
                        FailureCategory::Other => AccountStatus::Failed,
                    };
                    runtime.error_summary =
                        (failure.category != FailureCategory::Cancelled).then_some(failure.summary);
                    runtime.diagnostic = (failure.category != FailureCategory::Cancelled)
                        .then_some(failure.diagnostic);
                    runtime.login_challenge = None;
                    self.save_config();
                }
            }
        }

        if self.auto_refresh_pending
            && matches!(self.codex_state, CodexState::Available(_))
            && !self.worker.any_active()
        {
            self.auto_refresh_pending = false;
            self.refresh_all(ctx.clone());
        }
    }

    pub(crate) fn start_codex_detection(&mut self, ctx: egui::Context) {
        self.codex_state = CodexState::Detecting;
        self.worker
            .detect_codex(self.config.settings.custom_codex_path.clone(), ctx);
    }

    pub(crate) fn refresh_all(&mut self, ctx: egui::Context) {
        let Some(path) = self.codex_path() else {
            self.global_message = Some("Codex 不可用，无法刷新账号".to_owned());
            return;
        };
        let accounts = self.config.accounts.clone();
        self.worker
            .refresh_all(accounts, path, self.request_timeout(), ctx);
    }

    pub(crate) fn refresh_one(&mut self, id: Uuid, ctx: egui::Context) {
        let Some(path) = self.codex_path() else {
            self.global_message = Some("Codex 不可用，无法刷新账号".to_owned());
            return;
        };
        let Some(account) = self
            .config
            .accounts
            .iter()
            .find(|account| account.id == id)
            .cloned()
        else {
            return;
        };
        self.worker
            .refresh_one(account, path, self.request_timeout(), ctx);
    }

    pub(crate) fn begin_login(&mut self, id: Uuid, device_code: bool, ctx: egui::Context) {
        let Some(path) = self.codex_path() else {
            self.global_message = Some("Codex 不可用，无法开始登录".to_owned());
            return;
        };
        let Some(account) = self
            .config
            .accounts
            .iter()
            .find(|account| account.id == id)
            .cloned()
        else {
            return;
        };
        if let Some(storage) = &self.storage
            && let Err(error) = storage.ensure_account_home(&account)
        {
            self.set_global_error(error);
            return;
        }
        self.worker
            .login(account, path, self.request_timeout(), device_code, ctx);
    }

    pub(crate) fn begin_logout(&mut self, id: Uuid, ctx: egui::Context) {
        let Some(path) = self.codex_path() else {
            self.global_message = Some("Codex 不可用，无法退出登录".to_owned());
            return;
        };
        let Some(account) = self
            .config
            .accounts
            .iter()
            .find(|account| account.id == id)
            .cloned()
        else {
            return;
        };
        self.worker
            .logout(account, path, self.request_timeout(), ctx);
    }

    pub(crate) fn add_account(&mut self, name: String, device_code: bool, ctx: egui::Context) {
        let Some(storage) = &self.storage else {
            self.global_message = Some("本地数据目录不可用，无法添加账号".to_owned());
            return;
        };
        let display_name = if name.trim().is_empty() {
            format!("账号 {}", self.config.accounts.len() + 1)
        } else {
            name.trim().to_owned()
        };
        match storage.new_account(display_name, self.config.accounts.len() as u32) {
            Ok(account) => {
                let id = account.id;
                self.runtimes.insert(id, AccountRuntime::default());
                self.config.accounts.push(account);
                self.save_config();
                self.add_dialog = None;
                self.begin_login(id, device_code, ctx);
            }
            Err(error) => self.set_global_error(error),
        }
    }

    pub(crate) fn rename_account(&mut self, id: Uuid, name: String) {
        if let Some(account) = self
            .config
            .accounts
            .iter_mut()
            .find(|account| account.id == id)
        {
            let name = name.trim();
            if !name.is_empty() {
                account.display_name = name.to_owned();
                self.save_config();
            }
        }
        self.edit_dialog = None;
    }

    pub(crate) fn set_account_enabled(&mut self, id: Uuid, enabled: bool) {
        if let Some(account) = self
            .config
            .accounts
            .iter_mut()
            .find(|account| account.id == id)
        {
            account.enabled = enabled;
            self.save_config();
        }
    }

    pub(crate) fn delete_account(&mut self, id: Uuid, delete_credentials: bool) {
        if self.worker.is_active(id) {
            self.global_message = Some("请先取消该账号正在进行的操作".to_owned());
            return;
        }
        let Some(index) = self
            .config
            .accounts
            .iter()
            .position(|account| account.id == id)
        else {
            self.delete_dialog = None;
            return;
        };
        let account = self.config.accounts[index].clone();
        if delete_credentials
            && let Some(storage) = &self.storage
            && let Err(error) = storage.delete_account_home(&account)
        {
            self.set_global_error(error);
            return;
        }
        self.config.accounts.remove(index);
        self.runtimes.remove(&id);
        self.reindex_accounts();
        self.save_config();
        self.delete_dialog = None;
    }

    pub(crate) fn apply_settings(&mut self, ctx: &egui::Context) {
        self.config.settings.custom_codex_path = (!self.settings_path_buffer.trim().is_empty())
            .then(|| PathBuf::from(self.settings_path_buffer.trim()));
        self.config.settings.request_timeout_seconds =
            self.config.settings.request_timeout_seconds.clamp(5, 120);
        self.config.settings.stale_after_minutes =
            self.config.settings.stale_after_minutes.clamp(1, 1_440);
        self.save_config();
        self.apply_theme(ctx);
        self.start_codex_detection(ctx.clone());
    }

    pub(crate) fn set_theme_preference(
        &mut self,
        preference: ThemePreference,
        ctx: &egui::Context,
    ) {
        if self.config.settings.theme == preference {
            return;
        }
        self.config.settings.theme = preference;
        self.apply_theme(ctx);
        self.save_config();
        ctx.request_repaint();
    }

    pub(crate) fn apply_theme(&self, ctx: &egui::Context) {
        let (egui_theme, window_theme) = theme_preferences(self.config.settings.theme);
        ctx.set_theme(egui_theme);
        ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(window_theme));
    }

    pub(crate) fn save_config(&mut self) {
        if let Some(storage) = &self.storage
            && let Err(error) = storage.save(&self.config)
        {
            self.set_global_error(error);
        }
    }

    fn codex_path(&self) -> Option<PathBuf> {
        match &self.codex_state {
            CodexState::Available(installation) => Some(installation.path.clone()),
            _ => None,
        }
    }

    fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.config.settings.request_timeout_seconds)
    }

    fn set_global_error(&mut self, error: AppError) {
        self.global_message = Some(error.user_message().to_owned());
        self.global_diagnostic = Some(error.diagnostic());
        crate::logging::warn(error.diagnostic());
    }

    fn reindex_accounts(&mut self) {
        for (index, account) in self.config.accounts.iter_mut().enumerate() {
            account.order = index as u32;
        }
    }
}

fn theme_preferences(preference: ThemePreference) -> (egui::ThemePreference, egui::SystemTheme) {
    match preference {
        ThemePreference::System => (
            egui::ThemePreference::System,
            egui::SystemTheme::SystemDefault,
        ),
        ThemePreference::Light => (egui::ThemePreference::Light, egui::SystemTheme::Light),
        ThemePreference::Dark => (egui::ThemePreference::Dark, egui::SystemTheme::Dark),
    }
}

impl eframe::App for MonitorApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.process_events(ui.ctx());
        crate::ui::render(self, ui, frame);
        if self.worker.any_active() {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        clear_color_for_theme(visuals)
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.worker.shutdown();
        self.save_config();
    }
}

fn clear_color_for_theme(visuals: &egui::Visuals) -> [f32; 4] {
    visuals.panel_fill.to_normalized_gamma_f32()
}

#[cfg(test)]
mod tests {
    use super::{clear_color_for_theme, theme_preferences};
    use crate::storage::ThemePreference;
    use eframe::egui;

    #[test]
    fn maps_saved_theme_to_egui_and_native_window_themes() {
        assert_eq!(
            theme_preferences(ThemePreference::System),
            (
                egui::ThemePreference::System,
                egui::SystemTheme::SystemDefault
            )
        );
        assert_eq!(
            theme_preferences(ThemePreference::Light),
            (egui::ThemePreference::Light, egui::SystemTheme::Light)
        );
        assert_eq!(
            theme_preferences(ThemePreference::Dark),
            (egui::ThemePreference::Dark, egui::SystemTheme::Dark)
        );
    }

    #[test]
    fn clear_color_follows_the_active_theme_background() {
        let light = egui::Visuals::light();
        let dark = egui::Visuals::dark();

        assert_eq!(
            clear_color_for_theme(&light),
            light.panel_fill.to_normalized_gamma_f32()
        );
        assert_eq!(
            clear_color_for_theme(&dark),
            dark.panel_fill.to_normalized_gamma_f32()
        );
        assert_ne!(clear_color_for_theme(&light), clear_color_for_theme(&dark));
    }
}
