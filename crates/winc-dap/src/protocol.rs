//! Typed DAP payloads. Unknown fields are preserved by the wire layer.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Adapter/client identity sent during initialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Configuration {
    /// Adapter identifier; clients may omit it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adapter_id: Option<String>,
    /// Whether the client sends columns in UTF-16 units.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_start_at_one: Option<bool>,
    /// Whether the client sends one-based columns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub columns_start_at_one: Option<bool>,
    /// Adapter-specific path formats.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_format: Option<String>,
    /// Additional client configuration.
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, Value>,
}

/// Initialize request arguments.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    /// Client identifier.
    pub client_id: String,
    /// Client display name.
    pub client_name: String,
    /// Adapter identifier.
    pub adapter_id: String,
    /// Locale.
    pub locale: String,
    /// One-based line convention.
    pub lines_start_at_one: bool,
    /// UTF-16 column convention.
    pub columns_start_at_one: bool,
    /// Supports variable paging.
    pub supports_variable_paging: bool,
    /// Supports delayed variable type reporting.
    pub supports_variable_type: bool,
    /// Path format understood by the client.
    pub path_format: String,
}

impl Default for InitializeRequest {
    fn default() -> Self {
        Self {
            client_id: "winc".into(),
            client_name: "Winc".into(),
            adapter_id: "lldb-vscode".into(),
            locale: "zh-CN".into(),
            lines_start_at_one: false,
            columns_start_at_one: false,
            supports_variable_paging: true,
            supports_variable_type: true,
            path_format: "path".into(),
        }
    }
}

/// Initialize result body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// Adapter capabilities.
    #[serde(default)]
    pub capabilities: Value,
}

/// Launch/attach program settings.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchRequest {
    /// Executable to debug.
    pub program: String,
    /// Optional working directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Program arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Environment additions.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
    /// Stop at program entry.
    pub stop_on_entry: bool,
    /// Adapter-specific launch options.
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, Value>,
}

/// Source file identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    /// Source name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Local source path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Stable adapter source reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_reference: Option<i64>,
}

/// One breakpoint accepted by the adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Breakpoint {
    /// Whether the breakpoint is active.
    #[serde(default)]
    pub verified: bool,
    /// Resolved line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Adapter message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `SetBreakpoints` request arguments.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetBreakpointsRequest {
    /// Source whose breakpoints are replaced.
    pub source: Source,
    /// Requested breakpoint locations.
    pub breakpoints: Vec<SourceBreakpoint>,
}

/// Requested breakpoint location.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceBreakpoint {
    /// Zero-based line.
    pub line: u32,
    /// Optional zero-based UTF-16 column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

/// `SetBreakpoints` response body.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetBreakpointsResponse {
    /// Breakpoints in request order.
    pub breakpoints: Vec<Breakpoint>,
}

/// Thread returned by the adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    /// Adapter thread identifier.
    pub id: i64,
    /// Display name.
    pub name: String,
}

/// Threads response body.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ThreadsResponse {
    /// Available threads.
    pub threads: Vec<Thread>,
}

/// One stack frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackFrame {
    /// Frame identifier used for scopes.
    pub id: i64,
    /// Function or method name.
    pub name: String,
    /// Optional source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// Zero-based line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Zero-based UTF-16 column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

/// `StackTrace` response body.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StackTraceResponse {
    /// Stack frames.
    pub stack_frames: Vec<StackFrame>,
    /// Total frame count when supplied.
    #[serde(default)]
    pub total_frames: Option<usize>,
}

/// One lexical scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    /// Adapter scope identifier used by variables.
    pub variables_reference: i64,
    /// Display name.
    pub name: String,
    /// Whether the scope is expensive to evaluate.
    #[serde(default)]
    pub expensive: bool,
}

/// Scopes response body.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ScopesResponse {
    /// Lexical scopes.
    pub scopes: Vec<Scope>,
}

/// One variable or child value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    /// Display name.
    pub name: String,
    /// Display value.
    pub value: String,
    /// Optional type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    /// Reference for requesting children; zero means a leaf.
    #[serde(default)]
    pub variables_reference: i64,
}

/// Variables response body.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct VariablesResponse {
    /// Variables in display order.
    pub variables: Vec<Variable>,
}

/// Stopped event body.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StoppedEvent {
    /// Stop reason such as breakpoint, step, or exception.
    pub reason: String,
    /// Optional human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Stopped thread identifier.
    #[serde(default)]
    pub thread_id: Option<i64>,
    /// Whether all threads stopped.
    #[serde(default)]
    pub all_threads_stopped: Option<bool>,
}

/// Output event body.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OutputEvent {
    /// Output category (console, stdout, stderr, etc.).
    pub category: String,
    /// Output text.
    pub output: String,
    /// Optional source location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// Optional zero-based line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Optional zero-based UTF-16 column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

/// Terminated event body.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TerminatedEvent {
    /// Optional adapter restart information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<Value>,
}

/// Strongly typed subset of incoming DAP messages.
#[derive(Debug, Clone, PartialEq)]
pub enum ProtocolMessage {
    /// Adapter initialized handshake/configuration response.
    Initialized(Value),
    /// Program launched.
    Launched(Value),
    /// Threads response.
    Threads(ThreadsResponse),
    /// Stack trace response.
    StackTrace(StackTraceResponse),
    /// Scopes response.
    Scopes(ScopesResponse),
    /// Variables response.
    Variables(VariablesResponse),
    /// Stopped event.
    Stopped(StoppedEvent),
    /// Output event.
    Output(OutputEvent),
    /// Terminated event.
    Terminated(TerminatedEvent),
    /// Other request response or adapter event.
    Other(Value),
}
