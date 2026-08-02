use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::rate_limits::QuotaSnapshot;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Kept only so schema v2 caches remain readable until the next refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub masked_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

#[cfg(test)]
mod tests {
    use super::AccountIdentity;

    #[test]
    fn reads_legacy_masked_email_cache_without_treating_it_as_full_email() {
        let identity: AccountIdentity = serde_json::from_value(serde_json::json!({
            "masked_email": "pe***@example.com",
            "plan_type": "plus"
        }))
        .unwrap();
        assert_eq!(identity.email, None);
        assert_eq!(identity.masked_email.as_deref(), Some("pe***@example.com"));
    }

    #[test]
    fn persists_full_email_for_account_card_display() {
        let identity = AccountIdentity {
            account_id: Some("acct_fixture".to_owned()),
            email: Some("person@example.com".to_owned()),
            masked_email: None,
            plan_type: Some("plus".to_owned()),
        };
        let value = serde_json::to_value(identity).unwrap();
        assert_eq!(value["email"], "person@example.com");
        assert_eq!(value["account_id"], "acct_fixture");
        assert!(value.get("masked_email").is_none());
    }
}
