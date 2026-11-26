//! JSON-RPC 2.0 messages shared by DAP clients.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC request identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    /// Numeric identifier.
    Number(i64),
    /// String identifier.
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

/// A JSON-RPC error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Error {
    /// Protocol error code.
    pub code: i64,
    /// Human-readable message.
    pub message: String,
    /// Optional structured data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}

/// Client request in DAP wire format (`seq`, `type`, `command`, `arguments`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Request {
    /// Monotonic sequence number, also used to match the response.
    #[serde(rename = "seq")]
    pub id: Id,
    /// Always `request`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Command name.
    #[serde(rename = "command")]
    pub method: String,
    /// Command arguments.
    #[serde(rename = "arguments")]
    pub params: Option<Value>,
}

impl Request {
    /// Creates a request.
    pub fn new(id: Id, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            id,
            kind: "request",
            method: method.into(),
            params,
        }
    }
}

/// DAP event or notification.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Notification {
    /// Monotonic sequence number.
    #[serde(rename = "seq")]
    pub seq: Id,
    /// Always `event`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Event name.
    #[serde(rename = "event")]
    pub event: String,
    /// Event payload.
    #[serde(rename = "body")]
    pub body: Option<Value>,
}

impl Notification {
    /// Creates an event.
    pub fn new(seq: Id, event: impl Into<String>, body: Option<Value>) -> Self {
        Self {
            seq,
            kind: "event",
            event: event.into(),
            body,
        }
    }
}

/// Adapter response.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    /// Request identifier.
    pub id: Id,
    /// Successful payload.
    #[serde(default)]
    pub result: Option<Value>,
    /// Error payload.
    #[serde(default)]
    pub error: Option<Error>,
}

/// Incoming JSON-RPC message.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// Response to a request.
    Response(Response),
    /// Adapter event.
    Event(Notification),
}

impl Message {
    /// Parses a JSON value as a response or event.
    ///
    /// # Errors
    /// Returns a description when the value is not a JSON-RPC response/event.
    pub fn from_value(value: Value) -> Result<Self, String> {
        match value.get("type").and_then(Value::as_str) {
            Some("response") => {
                let id = value
                    .get("request_seq")
                    .cloned()
                    .and_then(|value| serde_json::from_value(value).ok())
                    .ok_or("response has no request_seq")?;
                let success = value
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let body = value.get("body").cloned();
                let response = if success {
                    Response {
                        id,
                        result: Some(body.unwrap_or(Value::Null)),
                        error: None,
                    }
                } else {
                    let message = value
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("debug adapter request failed")
                        .to_owned();
                    Response {
                        id,
                        result: None,
                        error: Some(Error {
                            code: -32000,
                            message,
                            data: body,
                        }),
                    }
                };
                return Ok(Self::Response(response));
            }
            Some("event") => {
                let event = value
                    .get("event")
                    .and_then(Value::as_str)
                    .ok_or("event must be a string")?
                    .to_owned();
                let body = value.get("body").cloned();
                return Ok(Self::Event(Notification::new(Id::Number(0), event, body)));
            }
            _ => {}
        }
        let response = serde_json::from_value(value).map_err(|error| error.to_string())?;
        Ok(Self::Response(response))
    }
}

/// Error returned by a pending request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    /// Adapter returned a protocol error.
    Protocol(Error),
    /// Adapter connection closed.
    Closed,
    /// Request was cancelled locally.
    Cancelled,
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Protocol(error) => error.fmt(f),
            Self::Closed => f.write_str("debug adapter connection closed"),
            Self::Cancelled => f.write_str("debug adapter request cancelled"),
        }
    }
}

impl std::error::Error for RequestError {}
impl From<std::io::Error> for RequestError {
    fn from(error: std::io::Error) -> Self {
        Self::Protocol(crate::Error {
            code: -32000,
            message: error.to_string(),
            data: None,
        })
    }
}

/// Alias matching the JSON-RPC error response shape.
pub type JsonRpcError = Error;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_response_and_event() {
        let response = Message::from_value(json!({
            "seq": 1, "type": "response", "request_seq": 4, "success": true, "command": "threads"
        }))
        .unwrap();
        assert!(matches!(
            response,
            Message::Response(Response {
                id: Id::Number(4),
                ..
            })
        ));
        let event = Message::from_value(json!({
            "seq": 2, "type": "event", "event": "stopped", "body": {}
        }))
        .unwrap();
        assert!(matches!(event, Message::Event(Notification { event, .. }) if event == "stopped"));
    }
}
