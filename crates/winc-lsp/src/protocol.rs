//! LSP 协议数据结构：位置、文本文档、诊断、补全、跳转和悬停。
//!
//! 这里的位置行号从 0 开始，列是 UTF-16 code unit 数，而不是字节数或 Unicode `char` 数。
//! 与 winc 文本内核交互时必须使用 [`crate::position`] 的转换函数。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 文本中的零基位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Position {
    /// 从 0 开始的行号。
    pub line: u32,
    /// 从 0 开始的 UTF-16 code unit 列号。
    pub character: u32,
}

impl Position {
    /// 创建位置。
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }

    /// 把文本中的 `char` 偏移转换为 LSP 位置。
    ///
    /// 行由 LF、CRLF 和 CR 划分；返回的列是 UTF-16 单元数。
    pub fn from_char_offset(text: &str, offset: usize) -> Option<Self> {
        crate::position::char_position(text, offset)
    }

    /// 把 LSP 位置转换为文本中的 `char` 偏移。
    ///
    /// 目标列落在 UTF-16 代理对中间时返回 `None`。
    pub fn to_char_offset(self, text: &str) -> Option<usize> {
        crate::position::utf16_position(text, self.line, self.character)
    }

    /// 把 LSP 位置转换为文本中的 UTF-8 字节偏移。
    ///
    /// 目标列落在 UTF-16 代理对中间时返回 `None`。
    pub fn to_byte_offset(self, text: &str) -> Option<usize> {
        crate::position::utf16_position_byte_offset(text, self.line, self.character)
    }
}

/// 文本中的零基范围，起点必须不大于终点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Range {
    /// 起点。
    pub start: Position,
    /// 终点（不包含）。
    pub end: Position,
}

impl Range {
    /// 创建范围。
    pub const fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }

    /// 创建单点范围。
    pub const fn point(position: Position) -> Self {
        Self {
            start: position,
            end: position,
        }
    }

    /// 判断范围是否有效。
    pub fn is_valid(self) -> bool {
        self.start <= self.end
    }
}

/// 文本文档标识。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TextDocumentIdentifier {
    /// 文档 URI。
    pub uri: String,
}

impl TextDocumentIdentifier {
    /// 创建文档标识。
    pub fn new(uri: impl Into<String>) -> Self {
        Self { uri: uri.into() }
    }
}

/// 带版本号的文本文档标识。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionedTextDocumentIdentifier {
    /// 文档 URI。
    pub uri: String,
    /// 文档版本。
    pub version: i64,
}

impl VersionedTextDocumentIdentifier {
    /// 创建带版本号的文档标识。
    pub fn new(uri: impl Into<String>, version: i64) -> Self {
        Self {
            uri: uri.into(),
            version,
        }
    }
}

/// 打开给语言服务器的文本文档。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocumentItem {
    /// 文档 URI。
    pub uri: String,
    /// 文档语言 id；仓颉语言必须是精确的 `"Cangjie"`。
    pub language_id: String,
    /// 文档版本；每次修改递增。
    pub version: i64,
    /// 当前完整文本。
    pub text: String,
}

impl TextDocumentItem {
    /// 创建文本文档。
    pub fn new(
        uri: impl Into<String>,
        language_id: impl Into<String>,
        version: i64,
        text: impl Into<String>,
    ) -> Self {
        Self {
            uri: uri.into(),
            language_id: language_id.into(),
            version,
            text: text.into(),
        }
    }
}

/// 一次文本文档变化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocumentContentChangeEvent {
    /// 替换的范围；全量同步时为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
    /// 新文本；没有 `range` 时替换整个文档。
    pub text: String,
}

impl TextDocumentContentChangeEvent {
    /// 创建全量文本变化。
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            range: None,
            text: text.into(),
        }
    }
}

/// 客户端能力。
///
/// 默认声明本客户端支持的读取类能力，以及全量文本同步。字段保持为 JSON，便于适配不同服务器。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ClientCapabilities {
    /// 工作区能力。
    pub workspace: Value,
    /// 文本同步能力。
    pub text_document: Value,
    /// 窗口能力。
    pub window: Value,
    /// 通用实验能力。
    pub general: Value,
    /// 保留的扩展能力。
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}

impl Default for ClientCapabilities {
    fn default() -> Self {
        Self {
            workspace: serde_json::json!({}),
            text_document: serde_json::json!({
                "synchronization": { "dynamicRegistration": false, "willSave": false, "willSaveWaitUntil": false, "didSave": false, "save": { "includeText": false }, "change": 2 },
                "completion": { "dynamicRegistration": false, "completionItem": { "snippetSupport": false, "documentationFormat": ["markdown", "plaintext"] } },
                "definition": { "dynamicRegistration": false, "linkSupport": true },
                "references": { "dynamicRegistration": false },
                "hover": { "dynamicRegistration": false, "contentFormat": ["markdown", "plaintext"] }
            }),
            window: serde_json::json!({}),
            general: serde_json::json!({}),
            extensions: BTreeMap::new(),
        }
    }
}

/// 服务器初始化选项，内容由具体语言服务器定义。
///
/// 仓颉单文件模式使用 `{"singleFilePath": "file:///..."}`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InitializeOptions(pub Value);

impl Default for InitializeOptions {
    fn default() -> Self {
        Self(Value::Object(serde_json::Map::new()))
    }
}

impl From<Value> for InitializeOptions {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

/// 服务器信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// 服务器名称。
    pub name: String,
    /// 可选版本。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// 服务器能力；服务器可返回额外字段，调用方可通过 [`raw`](Self::raw) 检查。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerCapabilities {
    /// 位置编码名称，通常是 `"utf-16"`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_encoding: Option<String>,
    /// 文本同步能力。
    pub text_document_sync: Value,
    /// 补全提供器。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_provider: Option<Value>,
    /// 定义提供器。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition_provider: Option<Value>,
    /// 引用提供器。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub references_provider: Option<Value>,
    /// 悬停提供器。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hover_provider: Option<Value>,
    /// 所有未单独建模的服务器能力。
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
    /// 原始 JSON，包含被建模和未建模的全部字段。
    pub raw: Value,
}

impl ServerCapabilities {
    /// 是否支持全量文本同步（同步 kind 为 1）。
    pub fn supports_full_sync(&self) -> bool {
        match &self.text_document_sync {
            Value::Number(number) => number.as_i64() == Some(1),
            Value::Object(object) => object.get("change").and_then(Value::as_i64) == Some(1),
            _ => false,
        }
    }
}

/// 诊断信息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// 受影响的文本范围。
    pub range: Range,
    /// 严重程度：1 错误、2 警告、3 信息、4 提示；服务器也可返回其他值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<u8>,
    /// 诊断代码。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<Value>,
    /// 稳定诊断标识。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_description: Option<Value>,
    /// 诊断来源，例如 `"cangjie"`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// 人类可读消息。
    pub message: String,
    /// 标签列表。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<u8>,
    /// 关联信息。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_information: Option<Value>,
    /// 服务器扩展字段。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// 一个文档的完整诊断发布。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    /// 文档 URI。
    pub uri: String,
    /// 可选文档版本；服务器可能省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    /// 当前诊断列表。
    pub diagnostics: Vec<Diagnostic>,
}

/// 补全候选。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionItem {
    /// 插入/显示的标签。
    pub label: String,
    /// 补全种类，由 LSP 枚举定义。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<u8>,
    /// 候选详情。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// 候选文档。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation: Option<Value>,
    /// 废弃标记。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<bool>,
    /// 排序文本。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_text: Option<String>,
    /// 过滤文本。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_text: Option<String>,
    /// 插入文本。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insert_text: Option<String>,
    /// 插入文本格式：1 纯文本，2 片段。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insert_text_format: Option<u8>,
    /// 文本编辑。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_edit: Option<Value>,
    /// 附加文本编辑；第一版客户端不应用它们。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub additional_text_edits: Option<Vec<Value>>,
    /// 排序规则。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_text_rule: Option<Value>,
    /// 服务器扩展字段。
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// 位置链接，允许服务器返回目标选择范围和来源范围。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationLink {
    /// 来源选择范围。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_selection_range: Option<Range>,
    /// 目标文档 URI。
    pub target_uri: String,
    /// 目标文档中链接指向的完整范围。
    pub target_range: Range,
    /// 目标文档中应选择的范围。
    pub target_selection_range: Range,
}

/// 单个跳转位置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Location {
    /// 目标文档 URI。
    pub uri: String,
    /// 目标范围。
    pub range: Range,
}

/// `MarkupContent` 或纯字符串形式的悬停内容。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MarkedContent {
    /// Markdown 或纯文本字符串。
    String(String),
    /// 带 kind 的结构化内容。
    Markup {
        /// 内容格式，通常是 `"markdown"` 或 `"plaintext"`。
        kind: String,
        /// 内容正文。
        value: String,
    },
}

/// 悬停结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hover {
    /// 悬停内容，允许结构化内容数组。
    pub contents: Value,
    /// 可选范围。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
}

/// 客户端启动配置。
///
/// 默认参数为官方仓颉 `LSPServer` 的 stdio 启动参数；可继续追加参数、环境变量和工作区根 URI。
#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
    /// 服务器可执行文件。
    pub program: PathBuf,
    /// 启动参数。
    pub args: Vec<String>,
    /// 追加或覆盖的环境变量。
    pub env: Vec<(String, String)>,
    /// 从子进程环境移除的变量名。
    pub env_remove: Vec<String>,
    /// 工作区根 URI。
    pub root_uri: Option<String>,
    /// 工作区文件夹。
    pub workspace_folders: Vec<Value>,
    /// 初始化选项。
    pub initialization_options: InitializeOptions,
    /// 客户端能力。
    pub client_capabilities: ClientCapabilities,
    /// 日志目录；服务器若支持 `--log-path` 时使用。
    pub log_path: Option<PathBuf>,
    /// 是否自动补默认的仓颉启动参数。
    pub cangjie_defaults: bool,
}

impl ServerConfig {
    /// 创建配置。
    ///
    /// 默认追加 `--enable-log=true -V --disableAutoImport`；设置 [`log_path`](Self::log_path)
    /// 后会追加 `--log-path=<目录>`。
    pub fn new(program: impl AsRef<Path>) -> Self {
        Self {
            program: program.as_ref().to_path_buf(),
            args: Vec::new(),
            env: Vec::new(),
            env_remove: Vec::new(),
            root_uri: None,
            workspace_folders: Vec::new(),
            initialization_options: InitializeOptions::default(),
            client_capabilities: ClientCapabilities::default(),
            log_path: None,
            cangjie_defaults: true,
        }
    }

    /// 设置工作区根 URI。
    pub fn root_uri(mut self, root_uri: impl Into<String>) -> Self {
        self.root_uri = Some(root_uri.into());
        self
    }

    /// 追加启动参数。
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// 追加多个启动参数。
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// 设置子进程环境变量。
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// 批量设置子进程环境变量。
    pub fn envs<I, K, V>(mut self, envs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.env.extend(
            envs.into_iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
        self
    }

    /// 从子进程环境移除变量。
    pub fn env_remove(mut self, key: impl Into<String>) -> Self {
        self.env_remove.push(key.into());
        self
    }

    /// 设置日志目录。
    pub fn log_path(mut self, path: impl AsRef<Path>) -> Self {
        self.log_path = Some(path.as_ref().to_path_buf());
        self
    }

    /// 设置初始化选项。
    pub fn initialization_options(mut self, options: impl Into<InitializeOptions>) -> Self {
        self.initialization_options = options.into();
        self
    }

    /// 设置客户端能力。
    pub fn client_capabilities(mut self, capabilities: ClientCapabilities) -> Self {
        self.client_capabilities = capabilities;
        self
    }

    /// 添加工作区文件夹（值为 LSP `WorkspaceFolder` JSON 对象）。
    pub fn workspace_folder(mut self, folder: Value) -> Self {
        self.workspace_folders.push(folder);
        self
    }

    /// 是否自动添加官方仓颉 `LSPServer` 默认参数。
    pub fn cangjie_defaults(mut self, enabled: bool) -> Self {
        self.cangjie_defaults = enabled;
        self
    }
}
