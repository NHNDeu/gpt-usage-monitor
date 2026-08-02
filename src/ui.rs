use chrono::{DateTime, Local, Utc};
use eframe::egui::{self, Color32, RichText, Stroke, Vec2};
use uuid::Uuid;

use crate::account::{AccountRecord, AccountRuntime, AccountStatus};
use crate::app::{
    AddDialog, CodexState, DeleteDialog, DesktopAccountState, EditDialog, MonitorApp,
};
use crate::rate_limits::{QuotaSnapshot, QuotaWindow};
use crate::storage::ThemePreference;

enum UiAction {
    Refresh(Uuid),
    Login(Uuid, bool),
    Logout(Uuid),
    Cancel(Uuid),
    Rename(Uuid, String),
    Delete(Uuid),
    SetEnabled(Uuid, bool),
    SwitchDesktop(Uuid),
}

pub fn render(app: &mut MonitorApp, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
    ui.style_mut().spacing.item_spacing = Vec2::new(8.0, 8.0);

    render_platform_header(app, ui);

    egui::Frame::NONE
        .inner_margin(egui::Margin::same(18))
        .show(ui, |ui| {
            #[cfg(not(target_os = "macos"))]
            render_header(app, ui);
            render_codex_banner(app, ui);
            render_global_message(app, ui);
            ui.add_space(4.0);

            if app.config.accounts.is_empty() {
                render_empty_state(app, ui);
            } else {
                let accounts = app.config.accounts.clone();
                let mut actions = Vec::new();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for account in accounts {
                            let runtime =
                                app.runtimes.get(&account.id).cloned().unwrap_or_default();
                            if let Some(action) = render_account_card(app, ui, &account, &runtime) {
                                actions.push(action);
                            }
                            ui.add_space(10.0);
                        }
                        ui.add_space(8.0);
                        render_footer(app, ui);
                    });
                for action in actions {
                    apply_action(app, action, ui.ctx().clone());
                }
            }
        });

    render_add_dialog(app, ui.ctx());
    render_edit_dialog(app, ui.ctx());
    render_delete_dialog(app, ui.ctx());
    render_login_dialogs(app, ui.ctx());
    render_settings(app, ui.ctx());
}

#[cfg(target_os = "macos")]
fn render_platform_header(app: &mut MonitorApp, ui: &mut egui::Ui) {
    let fill = if ui.visuals().dark_mode {
        Color32::from_rgb(36, 36, 39)
    } else {
        Color32::from_rgb(237, 237, 240)
    };
    egui::Frame::NONE
        .fill(fill)
        .inner_margin(egui::Margin {
            left: 18,
            right: 18,
            top: 0,
            bottom: 12,
        })
        .show(ui, |ui| {
            let response = ui.allocate_response(
                Vec2::new(ui.available_width(), 24.0),
                egui::Sense::click_and_drag(),
            );
            if response.drag_started() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            render_header(app, ui);
        });
    ui.separator();
}

#[cfg(not(target_os = "macos"))]
fn render_platform_header(_app: &mut MonitorApp, _ui: &mut egui::Ui) {}

fn render_header(app: &mut MonitorApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            let title_size = if cfg!(target_os = "macos") {
                21.0
            } else {
                25.0
            };
            ui.label(RichText::new("Codex 额度监控").size(title_size).strong());
            ui.label(
                RichText::new("统一查看多个 ChatGPT 账号的官方 Codex 限额")
                    .size(13.0)
                    .color(ui.visuals().weak_text_color()),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(!app.worker.any_active(), egui::Button::new("⚙ 设置"))
                .clicked()
            {
                app.show_settings = true;
            }
            if ui
                .add_enabled(!app.worker.any_active(), egui::Button::new("＋ 添加账号"))
                .clicked()
            {
                app.add_dialog = Some(AddDialog::default());
            }
            let can_refresh = matches!(app.codex_state, CodexState::Available(_))
                && !app.config.accounts.is_empty()
                && !app.worker.any_active();
            if ui
                .add_enabled(can_refresh, egui::Button::new("↻ 刷新全部"))
                .clicked()
            {
                app.refresh_all(ui.ctx().clone());
            }
            if app.worker.any_active()
                && !app.worker.desktop_switch_active()
                && ui.button("■ 取消全部").clicked()
            {
                app.worker.cancel_all();
            }
        });
    });

    ui.horizontal(|ui| {
        let progress = app
            .config
            .accounts
            .iter()
            .filter(|account| app.worker.is_active(account.id))
            .count();
        if app.worker.desktop_switch_active() {
            ui.spinner();
            ui.label("正在安全切换桌面应用账号（凭据替换阶段不可取消）");
        } else if progress > 0 {
            ui.spinner();
            ui.label(format!("正在处理 {progress} 个账号（刷新全部时依次查询）"));
        } else if let Some(time) = app.last_global_refresh {
            ui.label(format!("最后刷新：{}", format_local_time(time)));
        } else {
            ui.label("尚未刷新");
        }
    });
    ui.add_space(6.0);
}

fn render_codex_banner(app: &mut MonitorApp, ui: &mut egui::Ui) {
    let (fill, stroke) = match &app.codex_state {
        CodexState::Available(_) => (
            Color32::from_rgb(35, 105, 75),
            Color32::from_rgb(70, 170, 115),
        ),
        CodexState::Detecting => (
            Color32::from_rgb(70, 80, 105),
            Color32::from_rgb(120, 135, 175),
        ),
        CodexState::Unavailable { .. } => (
            Color32::from_rgb(120, 65, 55),
            Color32::from_rgb(200, 100, 85),
        ),
    };
    egui::Frame::NONE
        .fill(fill.gamma_multiply(0.22))
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| match &app.codex_state {
                CodexState::Detecting => {
                    ui.spinner();
                    ui.label("正在检测 Codex CLI、版本和 App Server 能力…");
                }
                CodexState::Available(installation) => {
                    ui.label(RichText::new("✓ Codex 可用").strong());
                    ui.separator();
                    ui.label(&installation.version);
                    ui.separator();
                    ui.label(installation.path.display().to_string());
                    if app.config.settings.desktop_switch_enabled {
                        ui.separator();
                        match &app.desktop_account_state {
                            DesktopAccountState::Managed(id) => {
                                let name = app
                                    .config
                                    .accounts
                                    .iter()
                                    .find(|account| account.id == *id)
                                    .map(|account| account.display_name.as_str())
                                    .unwrap_or("已管理账号");
                                ui.label(format!("桌面账号：{name}"));
                            }
                            DesktopAccountState::Checking => {
                                ui.spinner();
                                ui.label("正在验证桌面账号");
                            }
                            DesktopAccountState::Unmanaged => {
                                ui.label("⚠ 桌面账号未受本应用管理");
                            }
                            DesktopAccountState::Missing => {
                                ui.label("桌面应用尚未登录");
                            }
                            DesktopAccountState::Error(summary) => {
                                ui.label(format!("⚠ 桌面账号验证失败：{summary}"));
                            }
                            DesktopAccountState::Disabled => {}
                        }
                    }
                }
                CodexState::Unavailable {
                    summary,
                    diagnostic,
                } => {
                    ui.label(RichText::new("⚠ Codex 不可用").strong());
                    ui.separator();
                    ui.label(summary);
                    if ui.small_button("查看诊断").clicked() {
                        app.global_diagnostic = Some(diagnostic.clone());
                    }
                    if ui.small_button("重新检测").clicked() {
                        app.start_codex_detection(ui.ctx().clone());
                    }
                }
            });
        });
}

fn render_global_message(app: &mut MonitorApp, ui: &mut egui::Ui) {
    if app.global_message.is_none() && app.global_diagnostic.is_none() {
        return;
    }
    ui.add_space(6.0);
    egui::Frame::NONE
        .fill(Color32::from_rgb(128, 78, 35).gamma_multiply(0.18))
        .stroke(Stroke::new(1.0, Color32::from_rgb(205, 140, 70)))
        .corner_radius(8)
        .inner_margin(10)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(app.global_message.as_deref().unwrap_or("诊断信息"));
                if ui.small_button("关闭").clicked() {
                    app.global_message = None;
                    app.global_diagnostic = None;
                }
            });
            if let Some(diagnostic) = &app.global_diagnostic {
                ui.collapsing("脱敏诊断信息", |ui| {
                    ui.add(egui::Label::new(RichText::new(diagnostic).monospace()).wrap());
                });
            }
        });
}

fn render_empty_state(app: &mut MonitorApp, ui: &mut egui::Ui) {
    ui.add_space(50.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("还没有账号").size(22.0).strong());
        ui.add_space(8.0);
        ui.label("每个账号都使用独立的 Codex 状态目录与官方登录流程。");
        ui.label("应用不会要求或接触你的 ChatGPT 密码。");
        ui.add_space(14.0);
        if ui.button("＋ 添加第一个账号").clicked() {
            app.add_dialog = Some(AddDialog::default());
        }
    });
}

fn render_account_card(
    app: &MonitorApp,
    ui: &mut egui::Ui,
    account: &AccountRecord,
    runtime: &AccountRuntime,
) -> Option<UiAction> {
    let mut action = None;
    egui::Frame::group(ui.style())
        .corner_radius(11)
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&account.display_name).size(18.0).strong());
                let status_color = status_color(&runtime.status, ui);
                ui.label(
                    RichText::new(runtime.status.label())
                        .strong()
                        .color(status_color),
                );
                if !account.enabled {
                    ui.label(RichText::new("已停用").color(ui.visuals().weak_text_color()));
                }
                if matches!(
                    app.desktop_account_state,
                    DesktopAccountState::Managed(id) if id == account.id
                ) {
                    ui.label(
                        RichText::new("当前桌面账号")
                            .strong()
                            .color(Color32::from_rgb(65, 175, 115)),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut enabled = account.enabled;
                    if ui.checkbox(&mut enabled, "启用").changed() {
                        action = Some(UiAction::SetEnabled(account.id, enabled));
                    }
                });
            });

            if let Some(cache) = &account.last_success {
                ui.horizontal_wrapped(|ui| {
                    if let Some(email) = cache.identity.email.as_deref() {
                        ui.label("邮箱账号：");
                        ui.add(egui::Label::new(RichText::new(email).strong()).selectable(true));
                        if ui.small_button("复制邮箱").clicked() {
                            ui.ctx().copy_text(email.to_owned());
                        }
                    } else if let Some(masked) = cache.identity.masked_email.as_deref() {
                        ui.label(format!("邮箱账号：{masked}（旧缓存，刷新后显示完整邮箱）"));
                    } else {
                        ui.label("邮箱账号：官方未提供");
                    }
                    ui.separator();
                    ui.label("账号类型：ChatGPT");
                    ui.separator();
                    ui.label(format!(
                        "套餐：{}",
                        plan_label(cache.identity.plan_type.as_deref())
                    ));
                    ui.separator();
                    let stale = is_stale(
                        cache.quota.queried_at,
                        app.config.settings.stale_after_minutes,
                    );
                    let cache_label = if stale {
                        "⚠ 缓存数据 · 已过期"
                    } else {
                        "缓存数据"
                    };
                    ui.label(
                        RichText::new(format!(
                            "{cache_label} · {}",
                            format_local_time(cache.quota.queried_at)
                        ))
                        .color(if stale {
                            Color32::from_rgb(220, 150, 60)
                        } else {
                            ui.visuals().weak_text_color()
                        }),
                    );
                });
                ui.add_space(8.0);
                render_quota(ui, &cache.quota);
            } else {
                ui.label(
                    RichText::new("暂无额度数据。请登录并刷新。")
                        .color(ui.visuals().weak_text_color()),
                );
            }

            if let Some(summary) = &runtime.error_summary {
                ui.add_space(5.0);
                ui.label(
                    RichText::new(format!("⚠ {summary}")).color(Color32::from_rgb(220, 120, 95)),
                );
            }
            if let Some(diagnostic) = &runtime.diagnostic {
                ui.collapsing("脱敏诊断信息", |ui| {
                    ui.add(egui::Label::new(RichText::new(diagnostic).monospace()).wrap());
                });
            }

            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                let active = app.worker.is_active(account.id);
                let globally_busy = app.worker.any_active();
                if active {
                    if ui.button("■ 取消").clicked() {
                        action = Some(UiAction::Cancel(account.id));
                    }
                } else if globally_busy {
                    ui.label(
                        RichText::new("其他账号操作进行中，账号操作已锁定")
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                } else {
                    if ui
                        .add_enabled(
                            account.enabled && matches!(app.codex_state, CodexState::Available(_)),
                            egui::Button::new("↻ 刷新"),
                        )
                        .clicked()
                    {
                        action = Some(UiAction::Refresh(account.id));
                    }
                    let login_label = if account.last_success.is_some() {
                        "浏览器重新登录"
                    } else {
                        "浏览器登录"
                    };
                    if ui.button(login_label).clicked() {
                        action = Some(UiAction::Login(account.id, false));
                    }
                    if ui.button("设备码登录").clicked() {
                        action = Some(UiAction::Login(account.id, true));
                    }
                    if ui.button("退出登录").clicked() {
                        action = Some(UiAction::Logout(account.id));
                    }
                    let already_desktop = matches!(
                        app.desktop_account_state,
                        DesktopAccountState::Managed(id) if id == account.id
                    );
                    let switch_ready = app.config.settings.desktop_switch_enabled
                        && account.last_success.is_some()
                        && has_ordinary_auth_file(account);
                    let switch_label = if already_desktop {
                        "当前已是该账号"
                    } else {
                        "切换到桌面应用"
                    };
                    if ui
                        .add_enabled(
                            switch_ready && !already_desktop,
                            egui::Button::new(switch_label),
                        )
                        .on_hover_text(if app.config.settings.desktop_switch_enabled {
                            "将安全关闭 Codex 桌面宿主、切换本机凭据并验证账号"
                        } else {
                            "请先在设置中启用桌面账号切换"
                        })
                        .clicked()
                    {
                        action = Some(UiAction::SwitchDesktop(account.id));
                    }
                    if ui.button("重命名").clicked() {
                        action = Some(UiAction::Rename(account.id, account.display_name.clone()));
                    }
                    if ui.button("删除").clicked() {
                        action = Some(UiAction::Delete(account.id));
                    }
                }
            });
        });
    action
}

fn render_quota(ui: &mut egui::Ui, quota: &QuotaSnapshot) {
    if quota.windows.is_empty() {
        ui.label("官方当前未返回有效额度窗口。");
    }
    for window in &quota.windows {
        render_quota_window(ui, window);
        ui.add_space(7.0);
    }

    if quota.rate_limit_reached_type.is_some() {
        ui.label(
            RichText::new("⚠ 官方服务标记该账号已达到额度限制")
                .strong()
                .color(Color32::from_rgb(220, 105, 85)),
        );
    }
    if quota.spend_control_reached == Some(true) {
        ui.label(
            RichText::new("⚠ 已达到工作区用量控制上限")
                .strong()
                .color(Color32::from_rgb(220, 105, 85)),
        );
    }
    if let Some(count) = quota.reset_credits_available {
        ui.label(format!("官方可用额度重置次数：{count}"));
    }
    if let Some(usage) = &quota.token_usage {
        ui.collapsing("Token 活动摘要（官方 account/usage/read）", |ui| {
            if let Some(tokens) = usage.lifetime_tokens {
                ui.label(format!("累计 Token 活动：{tokens}"));
            }
            if let Some(days) = usage.current_streak_days {
                ui.label(format!("当前连续使用：{days} 天"));
            }
            if usage.lifetime_tokens.is_none() && usage.current_streak_days.is_none() {
                ui.label("官方未返回可展示的摘要字段。");
            }
        });
    }
}

fn render_quota_window(ui: &mut egui::Ui, window: &QuotaWindow) {
    egui::Frame::NONE
        .fill(ui.visuals().faint_bg_color)
        .corner_radius(7)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&window.local_name).strong());
                if let Some(name) = &window.official_name {
                    ui.label(
                        RichText::new(format!("官方名称：{name}"))
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                }
                if window.exhausted {
                    ui.label(
                        RichText::new("已耗尽")
                            .strong()
                            .color(Color32::from_rgb(220, 95, 80)),
                    );
                }
            });
            let fraction = window.remaining_percent / 100.0;
            let fill = if fraction <= 0.1 {
                Color32::from_rgb(210, 75, 70)
            } else if fraction <= 0.3 {
                Color32::from_rgb(215, 145, 55)
            } else {
                Color32::from_rgb(55, 155, 105)
            };
            ui.add(
                egui::ProgressBar::new(fraction)
                    .fill(fill)
                    .text(format!("剩余 {:.1}%", window.remaining_percent)),
            );
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("已使用 {:.1}%", window.used_percent));
                ui.separator();
                ui.label(format!("剩余 {:.1}%", window.remaining_percent));
                if let Some(duration) = window.window_duration_minutes {
                    ui.separator();
                    ui.label(format!("窗口 {duration} 分钟"));
                }
            });
            match window.resets_at {
                Some(reset) => {
                    ui.label(format!(
                        "重置：{}（{}）",
                        format_local_time(reset),
                        countdown(reset)
                    ));
                }
                None => {
                    ui.label(
                        RichText::new("官方未提供重置时间").color(ui.visuals().weak_text_color()),
                    );
                }
            }
        });
}

fn render_footer(app: &MonitorApp, ui: &mut egui::Ui) {
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new("按需查询 · 不发送模型对话 · 不驻留 App Server · 本机凭据切换 · 无遥测")
                .small()
                .color(ui.visuals().weak_text_color()),
        );
        if let Some(storage) = &app.storage {
            ui.separator();
            ui.label(
                RichText::new(format!("数据：{}", storage.root.display()))
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
        }
    });
}

fn render_add_dialog(app: &mut MonitorApp, ctx: &egui::Context) {
    let Some(dialog) = &mut app.add_dialog else {
        return;
    };
    let mut open = true;
    let mut close_clicked = false;
    let mut decision = None;
    egui::Window::new("添加 ChatGPT 账号")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(470.0)
        .show(ctx, |ui| {
            ui.label("本地显示名称（不会发送给 OpenAI）");
            ui.text_edit_singleline(&mut dialog.name);
            ui.add_space(7.0);
            ui.label(
                RichText::new(
                    "🔒 密码只能输入在 OpenAI 官方登录页面。默认额度查询不会解析令牌；启用桌面账号切换后只在本机复制 auth.json 并解析必要身份字段。",
                )
                .color(Color32::from_rgb(80, 165, 120)),
            );
            ui.label("浏览器可能沿用当前 ChatGPT 会话；添加第二、第三个账号时，请在官方页面切换到正确账号。");
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("创建并用浏览器登录").clicked() {
                    decision = Some(false);
                }
                if ui.button("创建并用设备码登录").clicked() {
                    decision = Some(true);
                }
                if ui.button("取消").clicked() {
                    close_clicked = true;
                }
            });
        });
    if let Some(device_code) = decision {
        let name = dialog.name.clone();
        app.add_account(name, device_code, ctx.clone());
    } else if !open || close_clicked {
        app.add_dialog = None;
    }
}

fn render_edit_dialog(app: &mut MonitorApp, ctx: &egui::Context) {
    let Some(dialog) = &mut app.edit_dialog else {
        return;
    };
    let mut open = true;
    let mut close_clicked = false;
    let mut save = false;
    egui::Window::new("重命名账号")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.text_edit_singleline(&mut dialog.name);
            ui.horizontal(|ui| {
                if ui.button("保存").clicked() {
                    save = true;
                }
                if ui.button("取消").clicked() {
                    close_clicked = true;
                }
            });
        });
    if save {
        let id = dialog.account_id;
        let name = dialog.name.clone();
        app.rename_account(id, name);
    } else if !open || close_clicked {
        app.edit_dialog = None;
    }
}

fn render_delete_dialog(app: &mut MonitorApp, ctx: &egui::Context) {
    let Some(dialog) = &mut app.delete_dialog else {
        return;
    };
    let mut open = true;
    let mut close_clicked = false;
    let mut confirm = false;
    let account = app
        .config
        .accounts
        .iter()
        .find(|account| account.id == dialog.account_id)
        .cloned();
    egui::Window::new("删除账号")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(480.0)
        .show(ctx, |ui| {
            if let Some(account) = &account {
                ui.label(format!("要删除“{}”的本地配置吗？", account.display_name));
                ui.checkbox(
                    &mut dialog.delete_credentials,
                    "同时永久删除该账号的 Codex 状态目录和登录凭据",
                );
                if dialog.delete_credentials {
                    ui.label(
                        RichText::new(format!("将永久删除：{}", account.state_dir.display()))
                            .color(Color32::from_rgb(220, 105, 85)),
                    );
                } else {
                    ui.label("账号目录会保留，可由你稍后手动删除。");
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new("确认删除")).clicked() {
                    confirm = true;
                }
                if ui.button("取消").clicked() {
                    close_clicked = true;
                }
            });
        });
    if confirm {
        let id = dialog.account_id;
        let delete_credentials = dialog.delete_credentials;
        app.delete_account(id, delete_credentials);
    } else if !open || close_clicked {
        app.delete_dialog = None;
    }
}

fn render_login_dialogs(app: &mut MonitorApp, ctx: &egui::Context) {
    let challenges: Vec<_> = app
        .runtimes
        .iter()
        .filter_map(|(id, runtime)| {
            runtime
                .login_challenge
                .clone()
                .map(|challenge| (*id, challenge))
        })
        .collect();
    for (account_id, challenge) in challenges {
        let name = app
            .config
            .accounts
            .iter()
            .find(|account| account.id == account_id)
            .map(|account| account.display_name.as_str())
            .unwrap_or("账号");
        egui::Window::new(format!("OpenAI 官方登录 · {name}"))
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.label("请只在浏览器打开的 OpenAI 官方页面输入凭据。");
                if challenge.device_code {
                    ui.label("设备码：");
                    ui.label(
                        RichText::new(challenge.user_code.as_deref().unwrap_or("未提供"))
                            .size(24.0)
                            .strong()
                            .monospace(),
                    );
                }
                ui.label("登录第二或第三个账号时，请确认官方页面当前选中的 ChatGPT 账号。");
                ui.horizontal(|ui| {
                    if ui.button("重新打开官方页面").clicked()
                        && let Err(error) = open::that(&challenge.url)
                    {
                        app.global_message =
                            Some("无法自动打开浏览器，请复制链接手动打开".to_owned());
                        app.global_diagnostic = Some(crate::logging::redact(&error.to_string()));
                    }
                    if ui.button("复制官方登录链接").clicked() {
                        ui.ctx().copy_text(challenge.url.clone());
                    }
                    if ui.button("取消登录").clicked() {
                        app.worker.cancel(account_id);
                    }
                });
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("等待官方登录完成通知…");
                });
            });
    }
}

fn render_settings(app: &mut MonitorApp, ctx: &egui::Context) {
    if !app.show_settings {
        return;
    }
    let mut open = true;
    let mut close_clicked = false;
    let mut apply = false;
    egui::Window::new("设置")
        .open(&mut open)
        .default_width(590.0)
        .resizable(true)
        .show(ctx, |ui| {
            ui.heading("Codex CLI");
            ui.label("自定义可执行文件路径（留空则搜索 PATH 和常见安装位置）");
            ui.text_edit_singleline(&mut app.settings_path_buffer);
            ui.label(
                RichText::new("Windows 建议选择实际的 codex.exe；路径可以包含空格。")
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
            ui.add_space(8.0);
            ui.heading("查询");
            ui.checkbox(
                &mut app.config.settings.auto_refresh_on_start,
                "启动后自动刷新全部已启用账号",
            );
            ui.add(
                egui::Slider::new(&mut app.config.settings.request_timeout_seconds, 5..=120)
                    .suffix(" 秒")
                    .text("单次请求超时"),
            );
            ui.add(
                egui::Slider::new(&mut app.config.settings.stale_after_minutes, 1..=1_440)
                    .suffix(" 分钟")
                    .text("缓存过期阈值"),
            );
            ui.add_space(8.0);
            ui.heading("桌面账号切换");
            ui.checkbox(
                &mut app.config.settings.desktop_switch_enabled,
                "启用“一键切换到 Codex 桌面应用”",
            );
            ui.label("全局 Codex Home（留空使用用户目录下的 .codex）");
            ui.add_enabled_ui(app.config.settings.desktop_switch_enabled, |ui| {
                ui.text_edit_singleline(&mut app.desktop_home_buffer);
            });
            if let Ok(path) = app.desktop_codex_home() {
                ui.label(
                    RichText::new(format!("当前解析路径：{}", path.display()))
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            }
            ui.label(
                RichText::new(
                    "切换会关闭并按需重启承载 Codex 的桌面宿主，正在运行的 Codex 任务会中断；普通聊天版 ChatGPT 不受影响。凭据只在本机复制，不上传。建议先结束正在运行的任务。",
                )
                .color(Color32::from_rgb(220, 145, 65)),
            );
            ui.label(
                RichText::new(
                    "仅支持文件型 auth.json；系统 Keychain/凭据管理器、Web Cookie 和某些嵌入页面会话不在本功能支持范围内。",
                )
                .small()
                .color(ui.visuals().weak_text_color()),
            );
            ui.add_space(8.0);
            ui.heading("外观");
            let mut theme_preference = app.config.settings.theme;
            egui::ComboBox::from_id_salt("theme-preference")
                .selected_text(match theme_preference {
                    ThemePreference::System => "跟随系统",
                    ThemePreference::Light => "浅色",
                    ThemePreference::Dark => "深色",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut theme_preference, ThemePreference::System, "跟随系统");
                    ui.selectable_value(&mut theme_preference, ThemePreference::Light, "浅色");
                    ui.selectable_value(&mut theme_preference, ThemePreference::Dark, "深色");
                });
            if theme_preference != app.config.settings.theme {
                app.set_theme_preference(theme_preference, ctx);
            }
            ui.label(
                RichText::new("主题选择会立即生效并保存。")
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
            ui.add_space(8.0);
            ui.heading("本地数据");
            if let Some(storage) = &app.storage {
                ui.label(storage.root.display().to_string());
                ui.label(
                    RichText::new("账号子目录包含由 Codex 官方管理的敏感凭据，请勿分享。")
                        .color(Color32::from_rgb(220, 145, 65)),
                );
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !app.worker.any_active(),
                        egui::Button::new("保存并重新检测 Codex"),
                    )
                    .clicked()
                {
                    apply = true;
                }
                if ui.button("关闭").clicked() {
                    close_clicked = true;
                }
            });
        });
    if apply {
        app.apply_settings(ctx);
    }
    if !open || close_clicked {
        app.show_settings = false;
    }
}

fn apply_action(app: &mut MonitorApp, action: UiAction, ctx: egui::Context) {
    match action {
        UiAction::Refresh(id) => app.refresh_one(id, ctx),
        UiAction::Login(id, device_code) => app.begin_login(id, device_code, ctx),
        UiAction::Logout(id) => app.begin_logout(id, ctx),
        UiAction::Cancel(id) => app.worker.cancel(id),
        UiAction::Rename(account_id, name) => {
            app.edit_dialog = Some(EditDialog { account_id, name });
        }
        UiAction::Delete(account_id) => {
            app.delete_dialog = Some(DeleteDialog {
                account_id,
                delete_credentials: false,
            });
        }
        UiAction::SetEnabled(id, enabled) => app.set_account_enabled(id, enabled),
        UiAction::SwitchDesktop(id) => app.begin_desktop_switch(id, ctx),
    }
}

fn has_ordinary_auth_file(account: &AccountRecord) -> bool {
    std::fs::symlink_metadata(account.state_dir.join("auth.json"))
        .is_ok_and(|metadata| metadata.file_type().is_file() && !metadata.file_type().is_symlink())
}

fn format_local_time(time: DateTime<Utc>) -> String {
    time.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S %:z")
        .to_string()
}

fn countdown(reset: DateTime<Utc>) -> String {
    let seconds = (reset - Utc::now()).num_seconds();
    if seconds <= 0 {
        return "已到重置时间，等待官方刷新".to_owned();
    }
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("还有 {days} 天 {hours} 小时")
    } else if hours > 0 {
        format!("还有 {hours} 小时 {minutes} 分钟")
    } else {
        format!("还有 {} 分钟", minutes.max(1))
    }
}

fn is_stale(time: DateTime<Utc>, stale_after_minutes: i64) -> bool {
    Utc::now().signed_duration_since(time).num_minutes() >= stale_after_minutes
}

fn plan_label(plan: Option<&str>) -> &str {
    match plan {
        Some("plus") => "ChatGPT Plus",
        Some("pro") => "ChatGPT Pro",
        Some("team") => "ChatGPT Team",
        Some("business") | Some("self_serve_business_usage_based") => "ChatGPT Business",
        Some("enterprise") | Some("enterprise_cbp_usage_based") => "ChatGPT Enterprise",
        Some("edu") => "ChatGPT Edu",
        Some("free") => "Free",
        Some("go") => "Go",
        Some("prolite") | Some("self_serve_business_prolite") => "Pro Lite",
        Some(_) => "未知套餐",
        None => "未提供",
    }
}

fn status_color(status: &AccountStatus, ui: &egui::Ui) -> Color32 {
    match status {
        AccountStatus::Success => Color32::from_rgb(65, 175, 115),
        AccountStatus::Querying(_) => Color32::from_rgb(95, 150, 225),
        AccountStatus::Idle => ui.visuals().weak_text_color(),
        AccountStatus::NotLoggedIn => Color32::from_rgb(220, 155, 65),
        AccountStatus::CodexUnavailable
        | AccountStatus::TimedOut
        | AccountStatus::ProtocolIncompatible
        | AccountStatus::Failed => Color32::from_rgb(220, 105, 85),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::{countdown, is_stale, plan_label};

    #[test]
    fn cache_staleness_and_countdown_are_explicit() {
        assert!(is_stale(Utc::now() - Duration::minutes(20), 15));
        assert!(!is_stale(Utc::now() - Duration::minutes(2), 15));
        assert!(countdown(Utc::now() + Duration::hours(2)).contains("小时"));
    }

    #[test]
    fn plan_names_are_human_readable_and_unknown_safe() {
        assert_eq!(plan_label(Some("plus")), "ChatGPT Plus");
        assert_eq!(plan_label(Some("future_plan")), "未知套餐");
        assert_eq!(plan_label(None), "未提供");
    }
}
