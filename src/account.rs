use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::rate_limits::QuotaSnapshot;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountIdentity {
    pub masked_email: Option<String>,
    pub plan_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedAccountData {
    pub identity: AccountIdentity,
    pub quota: QuotaSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountRecord {
    pub id: Uuid,
    pub display_name: String,
    pub state_dir: PathBuf,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub order: u32,
    #[serde(default)]
    pub last_success: Option<CachedAccountData>,
    #[serde(default)]
    pub last_attempt_at: Option<DateTime<Utc>>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq)]
pub enum AccountStatus {
    Idle,
    Querying(&'static str),
    Success,
    NotLoggedIn,
    CodexUnavailable,
    TimedOut,
    ProtocolIncompatible,
    Failed,
}

impl AccountStatus {
    pub fn label(&self) -> &str {
        match self {
            Self::Idle => "○ 等待查询",
            Self::Querying(step) => step,
            Self::Success => "✓ 查询成功",
            Self::NotLoggedIn => "⚠ 未登录",
            Self::CodexUnavailable => "⚠ Codex 不可用",
            Self::TimedOut => "⚠ 请求超时",
            Self::ProtocolIncompatible => "⚠ 协议不兼容",
            Self::Failed => "⚠ 查询失败",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AccountRuntime {
    pub status: AccountStatus,
    pub error_summary: Option<String>,
    pub diagnostic: Option<String>,
    pub login_challenge: Option<LoginChallenge>,
}

impl Default for AccountRuntime {
    fn default() -> Self {
        Self {
            status: AccountStatus::Idle,
            error_summary: None,
            diagnostic: None,
            login_challenge: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoginChallenge {
    pub login_id: String,
    pub url: String,
    pub user_code: Option<String>,
    pub device_code: bool,
}

pub fn mask_email(email: &str) -> String {
    let Some((local, domain)) = email.split_once('@') else {
        return "已登录账号".to_owned();
    };
    let mut chars = local.chars();
    let first = chars.next().unwrap_or('*');
    let second = chars.next();
    match second {
        Some(second) => format!("{first}{second}***@{domain}"),
        None => format!("{first}***@{domain}"),
    }
}

#[cfg(test)]
mod tests {
    use super::mask_email;

    #[test]
    fn masks_email_without_using_it_as_identity() {
        assert_eq!(mask_email("person@example.com"), "pe***@example.com");
        assert_eq!(mask_email("x@example.com"), "x***@example.com");
    }
}
