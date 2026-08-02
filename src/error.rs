use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Codex 可执行文件不可用：{0}")]
    CodexUnavailable(String),
    #[error("Codex App Server 不支持当前账户协议：{0}")]
    ProtocolIncompatible(String),
    #[error("Codex App Server 返回错误：{0}")]
    Server(String),
    #[error("账号尚未登录")]
    NotLoggedIn,
    #[error("操作超时：{0}")]
    Timeout(String),
    #[error("操作已取消")]
    Cancelled,
    #[error("Codex App Server 提前退出：{0}")]
    ProcessExited(String),
    #[error("无法解析 App Server 响应：{0}")]
    InvalidResponse(String),
    #[error("本地数据错误：{0}")]
    Storage(String),
    #[error("桌面账号切换失败：{0}")]
    DesktopSwitch(String),
    #[error("系统错误：{0}")]
    Io(#[from] io::Error),
}

impl AppError {
    pub fn user_message(&self) -> &'static str {
        match self {
            Self::CodexUnavailable(_) => "未找到可用的 Codex CLI",
            Self::ProtocolIncompatible(_) => "Codex 版本与应用协议不兼容",
            Self::Server(_) => "Codex 服务返回错误",
            Self::NotLoggedIn => "账号尚未登录或登录已失效",
            Self::Timeout(_) => "请求超时，请检查网络后重试",
            Self::Cancelled => "操作已取消",
            Self::ProcessExited(_) => "Codex App Server 意外退出",
            Self::InvalidResponse(_) => "Codex 返回了无法识别的数据",
            Self::Storage(_) => "无法读取或保存本地数据",
            Self::DesktopSwitch(_) => "无法安全切换桌面应用账号",
            Self::Io(_) => "本地系统操作失败",
        }
    }

    pub fn diagnostic(&self) -> String {
        crate::logging::redact(&self.to_string())
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
