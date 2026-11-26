//! JSON-RPC 2.0 消息模型与序列化。
//!
//! JSON-RPC 的 `id` 只允许字符串或数字（或 null）；本实现把数字统一保存为
//! [`Id::Number(i64)`]，因为 LSP 客户端通常使用递增整数。字符串 id 也完整支持。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 请求标识。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    /// 数字 id。LSP 使用 i64 范围；JSON-RPC 允许更大数字，但客户端实现可限制范围。
    Number(i64),
    /// 字符串 id。
    String(String),
}

impl From<i64> for Id {
    fn from(value: i64) -> Self {
        Self::Number(value)
    }
}

impl From<String> for Id {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for Id {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(value) => value.fmt(f),
            Self::String(value) => f.write_str(value),
        }
    }
}

/// JSON-RPC 错误对象。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Error {
    /// 错误码。
    pub code: i64,
    /// 简短错误描述。
    pub message: String,
    /// 可选的结构化错误数据。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON-RPC 错误 {}：{}", self.code, self.message)
    }
}

impl std::error::Error for Error {}

impl std::fmt::Display for JsonRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for JsonRpcError {}

/// JSON-RPC 错误响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// JSON-RPC 版本，应为 `"2.0"`。
    pub jsonrpc: String,
    /// 与此响应匹配的请求 id。
    pub id: Id,
    /// 错误详情。
    pub error: Error,
}

/// 客户端发出的请求。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    /// JSON-RPC 版本，应为 `"2.0"`。
    pub jsonrpc: String,
    /// 请求 id。
    pub id: Id,
    /// 方法名。
    pub method: String,
    /// 方法参数。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    /// 创建 JSON-RPC 2.0 请求。
    pub fn new(id: Id, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
        }
    }

    /// 转换为可序列化的 JSON 值。
    ///
    /// # Errors
    ///
    /// 参数无法序列化为 JSON 时返回错误。
    pub fn to_value(&self) -> Result<Value, serde_json::Error> {
        serde_json::to_value(self)
    }
}

/// 单向通知，没有请求 id，也不期待响应。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Notification {
    /// JSON-RPC 版本，应为 `"2.0"`。
    pub jsonrpc: String,
    /// 方法名。
    pub method: String,
    /// 方法参数。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Notification {
    /// 创建 JSON-RPC 2.0 通知。
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
        }
    }

    /// 转换为可序列化的 JSON 值。
    ///
    /// # Errors
    ///
    /// 参数无法序列化为 JSON 时返回错误。
    pub fn to_value(&self) -> Result<Value, serde_json::Error> {
        serde_json::to_value(self)
    }
}

/// 服务器响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    /// JSON-RPC 版本，应为 `"2.0"`。
    pub jsonrpc: String,
    /// 请求 id。
    pub id: Id,
    /// 成功结果或错误，二者必有其一。
    #[serde(flatten)]
    pub payload: ResponsePayload,
}

/// 响应的成功或错误载荷。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponsePayload {
    /// 成功结果，允许为任意 JSON（包括 null）。
    Result(Value),
    /// 失败详情。
    Error(JsonRpcError),
}

/// 从服务器接收的一条 JSON-RPC 消息。
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// 服务器对客户端请求的响应。
    Response(Response),
    /// 服务器发起的请求。
    ServerRequest(Request),
    /// 服务器通知。
    Notification(Notification),
}

impl Message {
    /// 解析一个 JSON-RPC 消息值。
    ///
    /// # Errors
    ///
    /// JSON-RPC 版本错误、id 非法或结构无法识别时返回错误。
    pub fn parse(value: Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "JSON-RPC 消息必须是对象".to_owned())?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err("JSON-RPC 消息版本必须是 2.0".into());
        }
        if let Some(method) = object.get("method") {
            let method = method
                .as_str()
                .ok_or_else(|| "JSON-RPC method 必须是字符串".to_owned())?
                .to_owned();
            let params = object.get("params").cloned();
            if let Some(id) = object.get("id") {
                let id = serde_json::from_value::<Id>(id.clone())
                    .map_err(|_| "JSON-RPC id 必须是整数或字符串".to_owned())?;
                return Ok(Self::ServerRequest(Request {
                    jsonrpc: "2.0".into(),
                    id,
                    method,
                    params,
                }));
            }
            return Ok(Self::Notification(Notification {
                jsonrpc: "2.0".into(),
                method,
                params,
            }));
        }

        let id = serde_json::from_value::<Id>(
            object
                .get("id")
                .cloned()
                .ok_or_else(|| "JSON-RPC 响应缺少 id".to_owned())?,
        )
        .map_err(|_| "JSON-RPC id 必须是整数或字符串".to_owned())?;
        if let Some(result) = object.get("result") {
            if object.contains_key("error") {
                return Err("JSON-RPC 响应不能同时包含 result 和 error".into());
            }
            return Ok(Self::Response(Response {
                jsonrpc: "2.0".into(),
                id,
                payload: ResponsePayload::Result(result.clone()),
            }));
        }
        let error = serde_json::from_value::<Error>(
            object
                .get("error")
                .cloned()
                .ok_or_else(|| "JSON-RPC 响应缺少 result 或 error".to_owned())?,
        )
        .map_err(|error| format!("JSON-RPC error 格式错误：{error}"))?;
        Ok(Self::Response(Response {
            jsonrpc: "2.0".into(),
            id: id.clone(),
            payload: ResponsePayload::Error(JsonRpcError {
                jsonrpc: "2.0".into(),
                id: id.clone(),
                error,
            }),
        }))
    }

    /// 响应匹配到的请求 id；通知或服务器请求返回 `None`。
    pub fn response_id(&self) -> Option<&Id> {
        match self {
            Self::Response(response) => Some(&response.id),
            Self::ServerRequest(_) | Self::Notification(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_requests_responses_and_notifications() {
        let request = Message::parse(json!({
            "jsonrpc": "2.0", "id": 7, "method": "textDocument/hover", "params": {"textDocument": {"uri": "file:///a"}}
        })).unwrap();
        assert!(matches!(
            request,
            Message::ServerRequest(Request {
                id: Id::Number(7),
                ..
            })
        ));

        let response =
            Message::parse(json!({"jsonrpc": "2.0", "id": "seven", "result": null})).unwrap();
        assert_eq!(response.response_id(), Some(&Id::String("seven".into())));

        let notification = Message::parse(json!({"jsonrpc": "2.0", "method": "window/logMessage", "params": {"type": 3, "message": "hello"}})).unwrap();
        assert!(matches!(notification, Message::Notification(_)));
    }

    #[test]
    fn error_payload_round_trips() {
        let value = json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32603, "message": "boom", "data": {"why": 2}}});
        let Message::Response(response) = Message::parse(value.clone()).unwrap() else {
            panic!("expected response")
        };
        assert!(matches!(response.payload, ResponsePayload::Error(_)));
        assert_eq!(serde_json::to_value(response).unwrap(), value);
    }

    #[test]
    fn rejects_invalid_messages() {
        assert!(Message::parse(json!({"jsonrpc": "1.0", "id": 1, "result": null})).is_err());
        assert!(Message::parse(json!({"jsonrpc": "2.0", "id": 1.5, "result": null})).is_err());
        assert!(Message::parse(json!({"jsonrpc": "2.0", "id": 1, "result": null, "error": {"code": 1, "message": "x"}})).is_err());
        assert!(Message::parse(json!({"jsonrpc": "2.0", "id": 1})).is_err());
    }
}
