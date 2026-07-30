use serde_json::{Value, json};

use crate::error::{AppError, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum IncomingMessage {
    Response {
        id: Value,
        result: std::result::Result<Value, RpcError>,
    },
    Notification {
        method: String,
        params: Value,
    },
    ServerRequest {
        id: Value,
        method: String,
        params: Value,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: Option<i64>,
    pub message: String,
}

pub fn request(id: u64, method: &str, params: Option<Value>) -> Value {
    let mut value = json!({"id": id, "method": method});
    if let Some(params) = params {
        value["params"] = params;
    }
    value
}

pub fn notification(method: &str, params: Option<Value>) -> Value {
    let mut value = json!({"method": method});
    if let Some(params) = params {
        value["params"] = params;
    }
    value
}

pub fn parse_line(line: &str) -> Result<IncomingMessage> {
    let value: Value = serde_json::from_str(line).map_err(|error| {
        AppError::InvalidResponse(format!("非法 JSON：{error}；片段={}", safe_excerpt(line)))
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| AppError::InvalidResponse("协议消息不是 JSON 对象".to_owned()))?;

    if let Some(id) = object.get("id") {
        if let Some(method) = object.get("method").and_then(Value::as_str) {
            return Ok(IncomingMessage::ServerRequest {
                id: id.clone(),
                method: method.to_owned(),
                params: object.get("params").cloned().unwrap_or(Value::Null),
            });
        }

        if let Some(error) = object.get("error") {
            let rpc_error = RpcError {
                code: error.get("code").and_then(Value::as_i64),
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("未知 App Server 错误")
                    .to_owned(),
            };
            return Ok(IncomingMessage::Response {
                id: id.clone(),
                result: Err(rpc_error),
            });
        }

        return Ok(IncomingMessage::Response {
            id: id.clone(),
            result: Ok(object.get("result").cloned().unwrap_or(Value::Null)),
        });
    }

    if let Some(method) = object.get("method").and_then(Value::as_str) {
        return Ok(IncomingMessage::Notification {
            method: method.to_owned(),
            params: object.get("params").cloned().unwrap_or(Value::Null),
        });
    }

    Err(AppError::InvalidResponse(
        "协议消息既没有 id 也没有 method".to_owned(),
    ))
}

fn safe_excerpt(line: &str) -> String {
    let sanitized = crate::logging::redact(line);
    sanitized.chars().take(180).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{IncomingMessage, notification, parse_line, request};

    #[test]
    fn builds_wire_messages_without_jsonrpc_header() {
        assert_eq!(
            request(7, "account/rateLimits/read", None),
            json!({"id": 7, "method": "account/rateLimits/read"})
        );
        assert_eq!(
            notification("initialized", None),
            json!({"method": "initialized"})
        );
    }

    #[test]
    fn parses_response_notification_and_unknown_fields() {
        let response = parse_line(r#"{"id":1,"result":{"ok":true},"future":"ignored"}"#).unwrap();
        assert!(matches!(
            response,
            IncomingMessage::Response { result: Ok(_), .. }
        ));
        let notification =
            parse_line(r#"{"method":"account/updated","params":{"planType":"plus"}}"#).unwrap();
        assert!(matches!(notification, IncomingMessage::Notification { .. }));
    }

    #[test]
    fn rejects_partial_or_invalid_json() {
        assert!(parse_line(r#"{"id":1,"result":"#).is_err());
    }

    #[test]
    fn parses_server_errors_without_raw_payload_dependency() {
        let response =
            parse_line(r#"{"id":3,"error":{"code":-32600,"message":"not logged in"}}"#).unwrap();
        match response {
            IncomingMessage::Response {
                result: Err(error), ..
            } => {
                assert_eq!(error.code, Some(-32600));
            }
            _ => panic!("expected error response"),
        }
    }
}
