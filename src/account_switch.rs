#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use serde_json::Value;
use uuid::Uuid;

use crate::account::{AccountIdentity, AccountRecord};
use crate::error::{AppError, Result};

const AUTH_FILE: &str = "auth.json";
const MAX_AUTH_BYTES: u64 = 1_048_576;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialIdentity {
    pub account_id: Option<String>,
    pub email: Option<String>,
}

impl CredentialIdentity {
    pub fn matches_account(&self, account: &AccountIdentity) -> bool {
        if let (Some(expected), Some(actual)) = (&self.account_id, &account.account_id) {
            return expected == actual;
        }
        match (&self.email, &account.email) {
            (Some(expected), Some(actual)) => expected.eq_ignore_ascii_case(actual),
            _ => false,
        }
    }

    pub fn same_account(&self, other: &Self) -> bool {
        if let (Some(left), Some(right)) = (&self.account_id, &other.account_id) {
            return left == right;
        }
        match (&self.email, &other.email) {
            (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialOwner {
    Managed(Uuid),
    Unmanaged,
    Ambiguous,
    Missing,
}

#[derive(Debug, Clone)]
pub struct DesktopCredentialInspection {
    pub owner: CredentialOwner,
    pub identity: Option<CredentialIdentity>,
}

pub struct SwitchReceipt {
    global_auth_path: PathBuf,
    previous_global: Option<Vec<u8>>,
    pub recovery_path: Option<PathBuf>,
    pub previous_owner: CredentialOwner,
}

struct CredentialDocument {
    bytes: Vec<u8>,
    identity: CredentialIdentity,
}

struct CurrentCredentialDocument {
    bytes: Vec<u8>,
    identity: Option<CredentialIdentity>,
}

pub fn default_global_codex_home() -> Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".codex"))
        .ok_or_else(|| AppError::Storage("无法确定用户目录中的全局 Codex Home".to_owned()))
}

pub fn auth_path(codex_home: &Path) -> PathBuf {
    codex_home.join(AUTH_FILE)
}

pub fn validate_target(account: &AccountRecord) -> Result<CredentialIdentity> {
    ensure_ordinary_directory(&account.state_dir, "目标账号状态目录")?;
    let path = auth_path(&account.state_dir);
    let identity = read_chatgpt_credentials(&path)?.identity;
    set_file_private(&path)?;
    Ok(identity)
}

pub fn harden_auth_file(codex_home: &Path) -> Result<()> {
    ensure_private_directory(codex_home)?;
    let path = auth_path(codex_home);
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(AppError::Storage(format!(
            "凭据路径不是普通文件：{}",
            path.display()
        )));
    }
    set_file_private(&path)
}

pub fn inspect_global(
    global_home: &Path,
    accounts: &[AccountRecord],
) -> Result<DesktopCredentialInspection> {
    ensure_absolute_global_home(global_home)?;
    ensure_global_home_is_separate(global_home, accounts)?;
    ensure_supported_global_storage(global_home)?;
    let global_path = auth_path(global_home);
    if !global_path.exists() {
        return Ok(DesktopCredentialInspection {
            owner: CredentialOwner::Missing,
            identity: None,
        });
    }
    let document = read_current_credentials(&global_path)?;
    let owner = document
        .identity
        .as_ref()
        .map(|identity| match_managed_owner(identity, accounts))
        .unwrap_or(CredentialOwner::Unmanaged);
    Ok(DesktopCredentialInspection {
        owner,
        identity: document.identity,
    })
}

pub fn replace_global_credentials(
    target: &AccountRecord,
    accounts: &[AccountRecord],
    global_home: &Path,
    recovery_root: &Path,
) -> Result<SwitchReceipt> {
    ensure_absolute_global_home(global_home)?;
    ensure_global_home_is_separate(global_home, accounts)?;
    ensure_ordinary_directory(&target.state_dir, "目标账号状态目录")?;
    ensure_supported_global_storage(global_home)?;
    ensure_private_directory(global_home)?;
    let target_document = read_chatgpt_credentials(&auth_path(&target.state_dir))?;
    let global_path = auth_path(global_home);
    let current = if global_path.exists() {
        Some(read_current_credentials(&global_path)?)
    } else {
        None
    };
    let previous_owner = match &current {
        Some(document) => document
            .identity
            .as_ref()
            .map(|identity| match_managed_owner(identity, accounts))
            .unwrap_or(CredentialOwner::Unmanaged),
        None => CredentialOwner::Missing,
    };

    let (recovery_path, recovery_dir) = if let Some(document) = &current {
        let directory = create_recovery_directory(recovery_root)?;
        let path = directory.join(AUTH_FILE);
        atomic_replace(&path, &document.bytes)?;
        (Some(path), Some(directory))
    } else {
        (None, None)
    };

    if let (Some(document), CredentialOwner::Managed(owner_id)) = (&current, previous_owner) {
        let owner = accounts
            .iter()
            .find(|account| account.id == owner_id)
            .ok_or_else(|| AppError::Storage("已匹配账号在配置中消失".to_owned()))?;
        ensure_ordinary_directory(&owner.state_dir, "当前受管账号状态目录")?;
        let owner_path = auth_path(&owner.state_dir);
        if let Some(directory) = &recovery_dir
            && owner_path.exists()
        {
            let previous_managed = read_raw_auth_file(&owner_path)?;
            atomic_replace(
                &directory.join(format!("managed-{owner_id}-previous.json")),
                &previous_managed,
            )?;
        }
        atomic_replace(&owner_path, &document.bytes)?;
    }

    atomic_replace(&global_path, &target_document.bytes)?;

    Ok(SwitchReceipt {
        global_auth_path: global_path,
        previous_global: current.map(|document| document.bytes),
        recovery_path,
        previous_owner,
    })
}

pub fn rollback_global_credentials(receipt: &SwitchReceipt) -> Result<()> {
    match &receipt.previous_global {
        Some(bytes) => atomic_replace(&receipt.global_auth_path, bytes),
        None => {
            if receipt.global_auth_path.exists() {
                let metadata = fs::symlink_metadata(&receipt.global_auth_path)?;
                if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                    return Err(AppError::Storage("拒绝回滚非普通全局凭据文件".to_owned()));
                }
                fs::remove_file(&receipt.global_auth_path)?;
                sync_parent(&receipt.global_auth_path)?;
            }
            Ok(())
        }
    }
}

pub fn match_managed_owner(
    identity: &CredentialIdentity,
    accounts: &[AccountRecord],
) -> CredentialOwner {
    let mut stable_matches = Vec::new();
    if let Some(account_id) = &identity.account_id {
        for account in accounts {
            if ensure_ordinary_directory(&account.state_dir, "受管账号状态目录").is_ok()
                && let Ok(document) = read_chatgpt_credentials(&auth_path(&account.state_dir))
                && document.identity.account_id.as_ref() == Some(account_id)
            {
                stable_matches.push(account.id);
            }
        }
        return match stable_matches.as_slice() {
            [id] => CredentialOwner::Managed(*id),
            [] => CredentialOwner::Unmanaged,
            _ => CredentialOwner::Ambiguous,
        };
    }

    let Some(email) = &identity.email else {
        return CredentialOwner::Unmanaged;
    };
    let email_matches: Vec<_> = accounts
        .iter()
        .filter_map(|account| {
            ensure_ordinary_directory(&account.state_dir, "受管账号状态目录").ok()?;
            read_chatgpt_credentials(&auth_path(&account.state_dir))
                .ok()
                .filter(|document| {
                    document
                        .identity
                        .email
                        .as_ref()
                        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(email))
                })
                .map(|_| account.id)
        })
        .collect();
    match email_matches.as_slice() {
        [id] => CredentialOwner::Managed(*id),
        [] => CredentialOwner::Unmanaged,
        _ => CredentialOwner::Ambiguous,
    }
}

fn read_chatgpt_credentials(path: &Path) -> Result<CredentialDocument> {
    let bytes = read_raw_auth_file(path)?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::Storage(format!("凭据文件不是有效 JSON：{}", path.display())))?;
    if value.get("auth_mode").and_then(Value::as_str) != Some("chatgpt") {
        return Err(AppError::Storage(format!(
            "仅支持已完成 ChatGPT OAuth 的文件型凭据：{}",
            path.display()
        )));
    }
    let identity = extract_identity(&value).ok_or_else(|| {
        AppError::Storage(format!(
            "无法从凭据中提取稳定账号 ID 或经过验证的完整邮箱：{}",
            path.display()
        ))
    })?;
    Ok(CredentialDocument { bytes, identity })
}

fn read_current_credentials(path: &Path) -> Result<CurrentCredentialDocument> {
    let bytes = read_raw_auth_file(path)?;
    let identity = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(|value| value.get("auth_mode").and_then(Value::as_str) == Some("chatgpt"))
        .as_ref()
        .and_then(extract_identity);
    Ok(CurrentCredentialDocument { bytes, identity })
}

fn ensure_supported_global_storage(global_home: &Path) -> Result<()> {
    let config_path = global_home.join("config.toml");
    let Ok(metadata) = fs::metadata(&config_path) else {
        return Ok(());
    };
    if metadata.len() > MAX_AUTH_BYTES {
        return Ok(());
    }
    let Ok(config) = fs::read_to_string(&config_path) else {
        return Ok(());
    };
    let explicit_non_file = config.lines().any(|line| {
        let line = line.trim().to_ascii_lowercase();
        line.starts_with("cli_auth_credentials_store")
            && ["keyring", "keychain", "wincred", "credential_manager"]
                .iter()
                .any(|value| line.contains(value))
    });
    if explicit_non_file {
        Err(AppError::DesktopSwitch(
            "检测到全局 Codex 使用系统 Keychain/凭据管理器；当前版本只支持文件型 auth.json"
                .to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn read_raw_auth_file(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        AppError::Storage(format!("无法读取凭据文件 {}：{error}", path.display()))
    })?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(AppError::Storage(format!(
            "凭据路径不是普通文件：{}",
            path.display()
        )));
    }
    if metadata.len() > MAX_AUTH_BYTES {
        return Err(AppError::Storage(format!(
            "凭据文件大小异常：{}",
            path.display()
        )));
    }
    fs::read(path).map_err(AppError::Io)
}

fn extract_identity(value: &Value) -> Option<CredentialIdentity> {
    let tokens = value.get("tokens")?;
    let direct_account_id = tokens
        .get("account_id")
        .or_else(|| value.get("account_id"))
        .and_then(Value::as_str)
        .and_then(non_empty_owned);
    let claims = tokens
        .get("id_token")
        .and_then(Value::as_str)
        .and_then(decode_jwt_claims);
    let claim_account_id = claims.as_ref().and_then(|claims| {
        [
            claims.get("account_id"),
            claims.get("chatgpt_account_id"),
            claims
                .get("https://api.openai.com/auth")
                .and_then(|auth| auth.get("chatgpt_account_id")),
        ]
        .into_iter()
        .flatten()
        .find_map(Value::as_str)
        .and_then(non_empty_owned)
    });
    let email = claims.as_ref().and_then(|claims| {
        (claims.get("email_verified").and_then(Value::as_bool) == Some(true))
            .then(|| claims.get("email").and_then(Value::as_str))
            .flatten()
            .and_then(non_empty_owned)
    });
    let identity = CredentialIdentity {
        account_id: direct_account_id.or(claim_account_id),
        email,
    };
    (identity.account_id.is_some() || identity.email.is_some()).then_some(identity)
}

fn decode_jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&decoded).ok()
}

fn non_empty_owned(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn ensure_ordinary_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        AppError::Storage(format!(
            "{label}不存在或无法读取 {}：{error}",
            path.display()
        ))
    })?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(AppError::Storage(format!(
            "{label}不是普通目录：{}",
            path.display()
        )));
    }
    Ok(())
}

fn ensure_absolute_global_home(path: &Path) -> Result<()> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(AppError::DesktopSwitch(
            "全局 Codex Home 必须是绝对路径".to_owned(),
        ))
    }
}

fn ensure_global_home_is_separate(global_home: &Path, accounts: &[AccountRecord]) -> Result<()> {
    if accounts
        .iter()
        .any(|account| account.state_dir == global_home)
    {
        Err(AppError::DesktopSwitch(
            "全局 Codex Home 不能与任何受管账号目录相同".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn create_recovery_directory(root: &Path) -> Result<PathBuf> {
    ensure_private_directory(root)?;
    let name = format!(
        "{}-{}",
        Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
        Uuid::new_v4()
    );
    let directory = root.join(name);
    fs::create_dir(&directory)?;
    set_directory_private(&directory)?;
    Ok(directory)
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    set_directory_private(path)
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Storage("凭据路径没有父目录".to_owned()))?;
    ensure_private_directory(parent)?;
    let temp_path = parent.join(format!(".auth-switch-{}.tmp", Uuid::new_v4()));
    atomic_replace_with_temp(path, &temp_path, bytes)
}

fn atomic_replace_with_temp(path: &Path, temp_path: &Path, bytes: &[u8]) -> Result<()> {
    let mut temp_created = false;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temp_path)?;
        temp_created = true;
        set_file_private(temp_path)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        rename_replace(temp_path, path)?;
        set_file_private(path)?;
        sync_parent(path)
    })();
    if temp_created && temp_path.exists() {
        let _ = fs::remove_file(temp_path);
    }
    result
}

#[cfg(not(windows))]
fn rename_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn rename_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both buffers are valid, NUL-terminated UTF-16 paths for the duration of the call.
    let success = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if success == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Storage("凭据路径没有父目录".to_owned()))?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
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
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::{
        CredentialOwner, atomic_replace_with_temp, auth_path, inspect_global,
        replace_global_credentials, rollback_global_credentials, validate_target,
    };
    use crate::account::{AccountIdentity, AccountRecord};

    fn fixture(account_id: &str, email: &str) -> Vec<u8> {
        let claims = serde_json::json!({
            "email": email,
            "email_verified": true,
            "https://api.openai.com/auth": {"chatgpt_account_id": account_id}
        });
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        serde_json::to_vec(&serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "account_id": account_id,
                "id_token": format!("e30.{payload}.fixture-signature"),
                "access_token": "fixture-access-token",
                "refresh_token": "fixture-refresh-token"
            }
        }))
        .unwrap()
    }

    fn account(root: &std::path::Path, name: &str, index: u32) -> AccountRecord {
        let id = Uuid::new_v4();
        let state_dir = root.join(id.to_string());
        fs::create_dir(&state_dir).unwrap();
        AccountRecord {
            id,
            display_name: name.to_owned(),
            state_dir,
            enabled: true,
            order: index,
            last_success: None,
            last_attempt_at: None,
        }
    }

    #[test]
    fn rejects_missing_target_auth() {
        let dir = tempdir().unwrap();
        let account = account(dir.path(), "A", 0);
        assert!(validate_target(&account).is_err());
    }

    #[test]
    fn credential_identity_requires_stable_id_or_verified_full_email_match() {
        let dir = tempdir().unwrap();
        let account = account(dir.path(), "A", 0);
        fs::write(
            auth_path(&account.state_dir),
            fixture("acct-a", "a@example.com"),
        )
        .unwrap();
        let identity = validate_target(&account).unwrap();
        let matching = AccountIdentity {
            account_id: Some("acct-a".to_owned()),
            email: Some("different@example.com".to_owned()),
            masked_email: None,
            plan_type: None,
        };
        let mismatching = AccountIdentity {
            account_id: Some("acct-b".to_owned()),
            email: Some("a@example.com".to_owned()),
            masked_email: None,
            plan_type: None,
        };
        assert!(identity.matches_account(&matching));
        assert!(!identity.matches_account(&mismatching));
    }

    #[test]
    fn global_missing_is_reported_without_guessing() {
        let dir = tempdir().unwrap();
        let inspection = inspect_global(&dir.path().join("global"), &[]).unwrap();
        assert_eq!(inspection.owner, CredentialOwner::Missing);
    }

    #[test]
    fn missing_global_is_created_atomically_and_rollback_restores_absence() {
        let dir = tempdir().unwrap();
        let accounts_root = dir.path().join("accounts");
        fs::create_dir(&accounts_root).unwrap();
        let target = account(&accounts_root, "target", 0);
        let target_bytes = fixture("acct-target", "target@example.com");
        fs::write(auth_path(&target.state_dir), &target_bytes).unwrap();
        let global = dir.path().join("global");

        let receipt = replace_global_credentials(
            &target,
            std::slice::from_ref(&target),
            &global,
            &dir.path().join("recovery"),
        )
        .unwrap();
        assert_eq!(receipt.previous_owner, CredentialOwner::Missing);
        assert_eq!(fs::read(auth_path(&global)).unwrap(), target_bytes);
        rollback_global_credentials(&receipt).unwrap();
        assert!(!auth_path(&global).exists());
    }

    #[test]
    fn matches_managed_account_and_ignores_stale_local_marker() {
        let dir = tempdir().unwrap();
        let accounts_root = dir.path().join("accounts");
        fs::create_dir(&accounts_root).unwrap();
        let first = account(&accounts_root, "same name", 0);
        let second = account(&accounts_root, "same name", 1);
        fs::write(
            auth_path(&first.state_dir),
            fixture("acct-a", "a@example.com"),
        )
        .unwrap();
        fs::write(
            auth_path(&second.state_dir),
            fixture("acct-b", "b@example.com"),
        )
        .unwrap();
        let global = dir.path().join("global");
        fs::create_dir(&global).unwrap();
        fs::write(auth_path(&global), fixture("acct-a", "a@example.com")).unwrap();

        let inspection = inspect_global(&global, &[first.clone(), second]).unwrap();
        assert_eq!(inspection.owner, CredentialOwner::Managed(first.id));
    }

    #[test]
    fn unknown_global_is_backed_up_and_never_written_into_managed_home() {
        let dir = tempdir().unwrap();
        let accounts_root = dir.path().join("accounts");
        fs::create_dir(&accounts_root).unwrap();
        let target = account(&accounts_root, "target", 0);
        let other = account(&accounts_root, "other", 1);
        let target_bytes = fixture("acct-target", "target@example.com");
        let other_bytes = fixture("acct-other", "other@example.com");
        fs::write(auth_path(&target.state_dir), &target_bytes).unwrap();
        fs::write(auth_path(&other.state_dir), &other_bytes).unwrap();
        let global = dir.path().join("global");
        fs::create_dir(&global).unwrap();
        let unknown = fixture("acct-unknown", "unknown@example.com");
        fs::write(auth_path(&global), &unknown).unwrap();

        let receipt = replace_global_credentials(
            &target,
            &[target.clone(), other.clone()],
            &global,
            &dir.path().join("recovery"),
        )
        .unwrap();
        assert_eq!(receipt.previous_owner, CredentialOwner::Unmanaged);
        assert_eq!(fs::read(auth_path(&global)).unwrap(), target_bytes);
        assert_eq!(fs::read(auth_path(&other.state_dir)).unwrap(), other_bytes);
        assert_eq!(fs::read(receipt.recovery_path.unwrap()).unwrap(), unknown);
    }

    #[test]
    fn malformed_global_is_treated_as_unknown_and_backed_up_before_switch() {
        let dir = tempdir().unwrap();
        let accounts_root = dir.path().join("accounts");
        fs::create_dir(&accounts_root).unwrap();
        let target = account(&accounts_root, "target", 0);
        fs::write(
            auth_path(&target.state_dir),
            fixture("acct-target", "target@example.com"),
        )
        .unwrap();
        let global = dir.path().join("global");
        fs::create_dir(&global).unwrap();
        let malformed = b"{damaged credential";
        fs::write(auth_path(&global), malformed).unwrap();

        let receipt = replace_global_credentials(
            &target,
            std::slice::from_ref(&target),
            &global,
            &dir.path().join("recovery"),
        )
        .unwrap();
        assert_eq!(receipt.previous_owner, CredentialOwner::Unmanaged);
        assert_eq!(
            fs::read(receipt.recovery_path.as_ref().unwrap()).unwrap(),
            malformed
        );
        rollback_global_credentials(&receipt).unwrap();
        assert_eq!(fs::read(auth_path(&global)).unwrap(), malformed);
    }

    #[test]
    fn explicit_keyring_storage_is_rejected() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global");
        fs::create_dir(&global).unwrap();
        fs::write(
            global.join("config.toml"),
            "cli_auth_credentials_store = \"keyring\"\n",
        )
        .unwrap();
        assert!(inspect_global(&global, &[]).is_err());
    }

    #[test]
    fn managed_global_is_saved_back_before_replacement() {
        let dir = tempdir().unwrap();
        let accounts_root = dir.path().join("accounts");
        fs::create_dir(&accounts_root).unwrap();
        let target = account(&accounts_root, "target", 0);
        let current = account(&accounts_root, "current", 1);
        fs::write(
            auth_path(&target.state_dir),
            fixture("acct-target", "target@example.com"),
        )
        .unwrap();
        let old_current = fixture("acct-current", "current@example.com");
        fs::write(auth_path(&current.state_dir), &old_current).unwrap();
        let global = dir.path().join("global");
        fs::create_dir(&global).unwrap();
        let refreshed_current = fixture("acct-current", "new-current@example.com");
        fs::write(auth_path(&global), &refreshed_current).unwrap();

        replace_global_credentials(
            &target,
            &[target.clone(), current.clone()],
            &global,
            &dir.path().join("recovery"),
        )
        .unwrap();
        assert_eq!(
            fs::read(auth_path(&current.state_dir)).unwrap(),
            refreshed_current
        );
    }

    #[test]
    fn atomic_replace_succeeds_and_uses_private_permissions() {
        let dir = tempdir().unwrap();
        let destination = dir.path().join("auth.json");
        let temporary = dir.path().join("temp");
        fs::write(&destination, b"old").unwrap();
        atomic_replace_with_temp(&destination, &temporary, b"new").unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"new");
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn temp_write_failure_preserves_destination() {
        let dir = tempdir().unwrap();
        let destination = dir.path().join("auth.json");
        fs::write(&destination, b"old").unwrap();
        let temporary = dir.path().join("missing").join("temp");
        assert!(atomic_replace_with_temp(&destination, &temporary, b"new").is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"old");
    }

    #[test]
    fn rename_failure_does_not_replace_destination_target() {
        let dir = tempdir().unwrap();
        let destination = dir.path().join("auth.json");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("sentinel"), b"old").unwrap();
        let temporary = dir.path().join("temp");
        assert!(atomic_replace_with_temp(&destination, &temporary, b"new").is_err());
        assert_eq!(fs::read(destination.join("sentinel")).unwrap(), b"old");
    }
}
