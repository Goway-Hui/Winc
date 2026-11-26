//! 通用 Language Server Protocol 客户端。
//!
//! 通过子进程标准输入/输出与语言服务器通信，不绑定异步运行时。读取线程把消息交给调用方，
//! 调用方负责驱动请求并处理 [`Event`]。协议位置使用 LSP 规定的 UTF-16 单元；本仓库文本内核
//! 使用 `char` 偏移，边界转换见 [`position`]。
//!
//! ```no_run
//! use std::time::Duration;
//! use winc_lsp::{Client, InitializeResult, ServerConfig};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut client = Client::start(ServerConfig::new("example-language-server")
//!     .root_uri("file:///tmp/project"))?;
//! let initialize = client.initialize(Duration::from_secs(10))?;
//! let raw = initialize.recv_timeout(Duration::from_secs(10))?;
//! let capabilities: InitializeResult = serde_json::from_value(raw)?;
//! client.initialized(capabilities.server_info)?;
//! client.did_open("file:///tmp/project/main.cj", "Cangjie", 1, "main() {}")?;
//! let completion = client.completion(
//!     "file:///tmp/project/main.cj",
//!     winc_lsp::Position::new(0, 7),
//!     Duration::from_secs(5),
//! )?;
//! completion.cancel(); // 不需要时可以取消；drop Client 也会清理服务器进程。
//! # Ok(())
//! # }
//! ```

mod framing;
mod jsonrpc;
pub mod position;
mod protocol;
mod transport;

pub use framing::{DecodeError, FramedReader, FramedWriter, decode_message, encode_message};
pub use jsonrpc::{Error, Id, JsonRpcError, Message, Notification, Request, Response};
pub use position::{char_to_utf16, utf16_to_byte_offset, utf16_to_char};
pub use protocol::{
    ClientCapabilities, CompletionItem, Diagnostic, Diagnostics, Hover, InitializeOptions,
    Location, LocationLink, MarkedContent, Position, Range, ServerCapabilities, ServerConfig,
    ServerInfo, TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    VersionedTextDocumentIdentifier,
};
pub use transport::{
    Client, Event, InitializeResult, LanguageService, PendingRequest, RequestError,
};
