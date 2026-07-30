use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::account::AccountRecord;
use crate::error::{AppError, Result};

pub const CURRENT_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default = "default_true")]
    pub auto_refresh_on_start: bool,
    #[serde(default)]
    pub custom_codex_path: Option<PathBuf>,
    #[serde(default = "default_request_timeout")]
    pub request_timeout_seconds: u64,
    #[serde(default = "default_stale_minutes")]
    pub stale_after_minutes: i64,
    #[serde(default)]
    pub theme: ThemePreference,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            auto_refresh_on_start: true,
            custom_codex_path: None,
            request_timeout_seconds: default_request_timeout(),
            stale_after_minutes: default_stale_minutes(),
            theme: ThemePreference::System,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub settings: AppSettings,
    #[serde(default)]
    pub accounts: Vec<AccountRecord>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            settings: AppSettings::default(),
            accounts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Storage {
    pub root: PathBuf,
    config_path: PathBuf,
    accounts_root: PathBuf,
    logs_root: PathBuf,
}

impl Storage {
    pub fn discover() -> Result<Self> {
        let dirs = ProjectDirs::from("com", "NHNDeu", "CodexUsageMonitor")
            .ok_or_else(|| AppError::Storage("无法确定标准应用数据目录".to_owned()))?;
        Self::at(dirs.data_local_dir().to_owned())
    }

    pub fn at(root: PathBuf) -> Result<Self> {
        let accounts_root = root.join("accounts");
        let logs_root = root.join("logs");
        fs::create_dir_all(&accounts_root)?;
        fs::create_dir_all(&logs_root)?;
        set_directory_private(&root)?;
        set_directory_private(&accounts_root)?;
        set_directory_private(&logs_root)?;
        Ok(Self {
            config_path: root.join("config.json"),
            root,
            accounts_root,
            logs_root,
        })
    }

    pub fn logs_root(&self) -> &Path {
        &self.logs_root
    }

    pub fn load_or_default(&self) -> (AppConfig, Option<String>) {
        match self.load() {
            Ok(config) => (config, None),
            Err(error) => {
                crate::logging::warn(format!("配置加载失败：{}", error.diagnostic()));
                (
                    AppConfig::default(),
                    Some(format!(
                        "本地配置无法读取，已用空配置启动。原文件未被覆盖。\n{}",
                        error.diagnostic()
                    )),
                )
            }
        }
    }

    pub fn load(&self) -> Result<AppConfig> {
        if !self.config_path.exists() {
            return Ok(AppConfig::default());
        }
        let bytes = fs::read(&self.config_path)?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| AppError::Storage(format!("config.json 损坏：{error}")))?;
        migrate(value)
    }

    pub fn save(&self, config: &AppConfig) -> Result<()> {
        let mut stored = config.clone();
        stored.schema_version = CURRENT_SCHEMA_VERSION;
        let bytes = serde_json::to_vec_pretty(&stored)
            .map_err(|error| AppError::Storage(error.to_string()))?;
        let temp_path = self.config_path.with_extension("json.tmp");
        {
            let mut file = fs::File::create(&temp_path)?;
            set_file_private(&temp_path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        fs::rename(&temp_path, &self.config_path)?;
        set_file_private(&self.config_path)?;
        Ok(())
    }

    pub fn new_account(&self, display_name: String, order: u32) -> Result<AccountRecord> {
        let id = Uuid::new_v4();
        let state_dir = self.accounts_root.join(id.to_string());
        fs::create_dir(&state_dir)?;
        set_directory_private(&state_dir)?;
        Ok(AccountRecord {
            id,
            display_name,
            state_dir,
            enabled: true,
            order,
            last_success: None,
            last_attempt_at: None,
        })
    }

    pub fn ensure_account_home(&self, account: &AccountRecord) -> Result<()> {
        let expected = self.expected_account_home(account.id);
        if account.state_dir != expected {
            return Err(AppError::Storage(format!(
                "账号目录不在受管数据目录中：{}",
                account.state_dir.display()
            )));
        }
        fs::create_dir_all(&expected)?;
        set_directory_private(&expected)
    }

    pub fn delete_account_home(&self, account: &AccountRecord) -> Result<()> {
        let expected = self.expected_account_home(account.id);
        if account.state_dir != expected {
            return Err(AppError::Storage("拒绝删除不属于本应用的目录".to_owned()));
        }
        if expected.exists() {
            fs::remove_dir_all(&expected)?;
        }
        Ok(())
    }

    pub fn expected_account_home(&self, id: Uuid) -> PathBuf {
        self.accounts_root.join(id.to_string())
    }
}

fn migrate(mut value: Value) -> Result<AppConfig> {
    let schema = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if schema > CURRENT_SCHEMA_VERSION as u64 {
        return Err(AppError::Storage(format!(
            "配置版本 {schema} 高于应用支持的版本 {CURRENT_SCHEMA_VERSION}"
        )));
    }

    if schema < CURRENT_SCHEMA_VERSION as u64 {
        let object = value
            .as_object_mut()
            .ok_or_else(|| AppError::Storage("配置根节点不是对象".to_owned()))?;
        object.insert(
            "schema_version".to_owned(),
            Value::from(CURRENT_SCHEMA_VERSION),
        );
        if schema == 1 {
            object
                .entry("settings")
                .or_insert_with(|| serde_json::json!({}));
        }
    }

    serde_json::from_value(value)
        .map_err(|error| AppError::Storage(format!("配置字段不兼容：{error}")))
}

fn default_true() -> bool {
    true
}

fn default_request_timeout() -> u64 {
    20
}

fn default_stale_minutes() -> i64 {
    15
}

#[cfg(unix)]
fn set_directory_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_directory_private(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_file_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_private(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::{AppConfig, CURRENT_SCHEMA_VERSION, Storage, migrate};
    use crate::account::AccountRecord;

    #[test]
    fn migrates_schema_v1_and_fills_defaults() {
        let id = Uuid::new_v4();
        let value = serde_json::json!({
            "schema_version": 1,
            "accounts": [{
                "id": id,
                "display_name": "主账号",
                "state_dir": "/tmp/example"
            }]
        });
        let config = migrate(value).unwrap();
        assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
        assert!(config.settings.auto_refresh_on_start);
        assert!(config.accounts[0].enabled);
    }

    #[test]
    fn migrates_schema_v2_for_full_email_cache_support() {
        let value = serde_json::json!({
            "schema_version": 2,
            "settings": {},
            "accounts": []
        });
        let config = migrate(value).unwrap();
        assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn saves_and_loads_atomically() {
        let dir = tempdir().unwrap();
        let storage = Storage::at(dir.path().join("data")).unwrap();
        let mut config = AppConfig::default();
        config
            .accounts
            .push(storage.new_account("账号 A".to_owned(), 0).unwrap());
        storage.save(&config).unwrap();
        let loaded = storage.load().unwrap();
        assert_eq!(loaded.accounts.len(), 1);
        assert_eq!(loaded.accounts[0].display_name, "账号 A");
    }

    #[test]
    fn account_homes_are_isolated_and_deletion_is_scoped() {
        let dir = tempdir().unwrap();
        let storage = Storage::at(dir.path().join("data")).unwrap();
        let first = storage.new_account("A".to_owned(), 0).unwrap();
        let second = storage.new_account("B".to_owned(), 1).unwrap();
        assert_ne!(first.state_dir, second.state_dir);

        storage.delete_account_home(&first).unwrap();
        assert!(!first.state_dir.exists());
        assert!(second.state_dir.exists());

        let outside = AccountRecord {
            state_dir: dir.path().join("outside"),
            ..second
        };
        assert!(storage.delete_account_home(&outside).is_err());
    }
}
