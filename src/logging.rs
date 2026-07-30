use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::Utc;

const MAX_LOG_BYTES: u64 = 1_000_000;
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static LOG_LOCK: Mutex<()> = Mutex::new(());

pub fn init(dir: &Path) {
    if fs::create_dir_all(dir).is_ok() {
        let _ = LOG_PATH.set(dir.join("codex-usage-monitor.log"));
    }
}

pub fn info(message: impl AsRef<str>) {
    write("INFO", message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    write("WARN", message.as_ref());
}

fn write(level: &str, message: &str) {
    let Some(path) = LOG_PATH.get() else {
        return;
    };
    let Ok(_guard) = LOG_LOCK.lock() else {
        return;
    };

    if fs::metadata(path).is_ok_and(|meta| meta.len() >= MAX_LOG_BYTES) {
        let rotated = path.with_extension("log.1");
        let _ = fs::remove_file(&rotated);
        let _ = fs::rename(path, rotated);
    }

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let safe = redact(message);
        let _ = writeln!(file, "{} [{}] {}", Utc::now().to_rfc3339(), level, safe);
    }
}

pub fn redact(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for token in input.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        let sensitive = lower.contains("access_token")
            || lower.contains("refresh_token")
            || lower.contains("authorization:")
            || lower.starts_with("bearer")
            || lower.starts_with("sk-")
            || (token.matches('.').count() == 2 && token.len() > 40);
        if sensitive {
            output.push_str("[REDACTED]");
        } else if token.contains('@') {
            output.push_str(&mask_email_token(token));
        } else {
            output.push_str(token);
        }
        output.push(' ');
    }
    output.trim_end().to_owned()
}

fn mask_email_token(token: &str) -> String {
    let Some((local, domain)) = token.split_once('@') else {
        return "[REDACTED_EMAIL]".to_owned();
    };
    let prefix: String = local.chars().take(1).collect();
    format!("{prefix}***@{domain}")
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn redacts_tokens_and_email() {
        let text = redact("user@example.com access_token=secret sk-secret");
        assert!(text.contains("u***@example.com"));
        assert!(!text.contains("secret"));
    }
}
