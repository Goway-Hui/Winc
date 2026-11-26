//! 同步 stdio 传输与客户端生命周期。

use std::collections::HashMap;
use std::io::{self, BufRead, BufWriter};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::framing::{DecodeError, FramedReader, FramedWriter};
use crate::jsonrpc::{Id, JsonRpcError, Message, Notification, Request};
use crate::protocol::{
    Diagnostic, Diagnostics, InitializeOptions, Position, ServerCapabilities, ServerConfig,
    ServerInfo, TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    VersionedTextDocumentIdentifier,
};

const DROP_EXIT_TIMEOUT: Duration = Duration::from_millis(500);

/// 语言服务器事件。
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// 服务器发布的诊断。
    Diagnostics {
        /// 文档 URI。
        uri: String,
        /// 可选文档版本。
        version: Option<i64>,
        /// 诊断列表。
        diagnostics: Vec<Diagnostic>,
    },
    /// 服务器日志消息，已保留完整 params JSON。
    LogMessage {
        /// 日志级别：1 错误、2 警告、3 信息、4 日志。
        level: Option<u8>,
        /// 日志正文。
        message: String,
        /// 原始参数。
        params: Option<Value>,
    },
    /// 服务器通知客户端退出；参数为 `exit` 通知的原始 params。
    Exit {
        /// 退出通知参数；通常为 null 或不存在。
        params: Option<Value>,
    },
    /// 服务器发起的请求。客户端必须回答，或显式拒绝。
    ServerRequest(Request),
    /// 其他通知，保留完整 method 和 params。
    Notification {
        /// 方法名。
        method: String,
        /// 原始参数。
        params: Option<Value>,
    },
    /// 服务器标准错误输出（按行）。
    Stderr(String),
    /// 消息格式或 JSON-RPC 结构错误。
    ProtocolError(String),
    /// 读取线程结束；`None` 表示 EOF，`Some` 表示协议或 I/O 错误。
    Closed(Option<String>),
}

impl Event {
    fn from_notification(notification: Notification) -> Option<Self> {
        let method = notification.method.as_str();
        let params = notification.params;
        match method {
            "textDocument/publishDiagnostics" => {
                let diagnostics = params
                    .as_ref()
                    .and_then(|value| serde_json::from_value::<Diagnostics>(value.clone()).ok());
                if let Some(value) = diagnostics {
                    return Some(Self::Diagnostics {
                        uri: value.uri,
                        version: value.version,
                        diagnostics: value.diagnostics,
                    });
                }
            }
            "window/logMessage" => {
                let level = params
                    .as_ref()
                    .and_then(|value| value.get("type"))
                    .and_then(Value::as_u64)
                    .and_then(|value| u8::try_from(value).ok());
                let message = params
                    .as_ref()
                    .and_then(|value| value.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                return Some(Self::LogMessage {
                    level,
                    message,
                    params,
                });
            }
            "exit" => return Some(Self::Exit { params }),
            _ => {}
        }
        Some(Self::Notification {
            method: notification.method,
            params,
        })
    }
}

type PendingMap = Arc<Mutex<HashMap<Id, std::sync::mpsc::Sender<Result<Value, JsonRpcError>>>>>;

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("closed", &self.closed.load(Ordering::Acquire))
            .field("exited", &self.exited.load(Ordering::Acquire))
            .field("initialized", &self.initialized.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}
/// 通用语言服务客户端。
///
/// `Client` 拥有子进程 stdin/stdout 管理、读取线程和未完成请求表。它不绑定任何 GUI；
/// 调用方可以用 [`take_events`](Self::take_events) 把事件接收器交给自己的事件循环。
pub struct Client {
    child: Arc<Mutex<Child>>,
    writer: Arc<Mutex<FramedWriter<BufWriter<ChildStdin>>>>,
    events: Arc<Mutex<std::sync::mpsc::Receiver<Event>>>,
    pending: PendingMap,
    cancelled: Arc<Mutex<HashMap<Id, Arc<AtomicBool>>>>,
    next_id: AtomicI64,
    closed: Arc<AtomicBool>,
    reader_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
    initialized: Arc<AtomicBool>,
    shutdown_sent: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
    initialize_state: InitializeState,
}

/// 尚未完成的请求句柄。
///
/// 接收线程会把匹配的响应放入独立通道；取消只是让 [`recv`](Self::recv) 和
/// [`recv_timeout`](Self::recv_timeout) 立即返回 [`RequestError::Cancelled`]，不会删除服务器端工作。
#[derive(Debug)]
pub struct PendingRequest {
    id: Id,
    receiver: std::sync::mpsc::Receiver<Result<Value, JsonRpcError>>,
    cancelled: Arc<AtomicBool>,
}

/// 等待请求结果时的错误。
#[derive(Debug, Clone, PartialEq)]
pub enum RequestError {
    /// 请求已取消。
    Cancelled,
    /// 在超时前没有收到响应。
    Timeout,
    /// 事件接收器已断开，通常客户端或进程已经关闭。
    Disconnected,
    /// 服务器返回 JSON-RPC 错误。
    Rpc(JsonRpcError),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("请求已取消"),
            Self::Timeout => f.write_str("等待语言服务器响应超时"),
            Self::Disconnected => f.write_str("语言服务器连接已关闭"),
            Self::Rpc(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RequestError {}

impl PendingRequest {
    /// 请求 id。
    pub const fn id(&self) -> &Id {
        &self.id
    }

    /// 取消等待；不会向服务器发送 `$/cancelRequest`。
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// 判断是否已请求取消。
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// 阻塞等待响应。
    ///
    /// # Errors
    ///
    /// 取消、超时（仅 [`recv_timeout`](Self::recv_timeout)）、断开或服务器返回错误时返回相应错误。
    pub fn recv(self) -> Result<Value, RequestError> {
        if self.is_cancelled() {
            return Err(RequestError::Cancelled);
        }
        match self.receiver.recv() {
            Ok(result) => result.map_err(RequestError::Rpc),
            Err(_) => Err(RequestError::Disconnected),
        }
    }

    /// 在指定时间内等待响应。
    ///
    /// # Errors
    ///
    /// 取消、超时、断开或服务器返回错误时返回相应错误。
    pub fn recv_timeout(self, timeout: Duration) -> Result<Value, RequestError> {
        let started = std::time::Instant::now();
        if self.is_cancelled() {
            return Err(RequestError::Cancelled);
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(RequestError::Timeout);
        }
        match self.receiver.recv_timeout(remaining) {
            Ok(result) => result.map_err(RequestError::Rpc),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(RequestError::Timeout),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(RequestError::Disconnected),
        }
    }

    /// 尝试非阻塞接收响应。
    ///
    /// # Errors
    ///
    /// 已取消或通道断开时返回相应错误；没有响应时返回 `Ok(None)`。
    pub fn try_recv(&mut self) -> Result<Option<Value>, RequestError> {
        if self.is_cancelled() {
            return Err(RequestError::Cancelled);
        }
        match self.receiver.try_recv() {
            Ok(result) => result.map(Some).map_err(RequestError::Rpc),
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(None),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(RequestError::Disconnected),
        }
    }
}

/// 兼容名称：表示一个已启动的语言服务。
///
/// 当前实现返回 [`Client`]，没有单独的 reader 回调层。
pub type LanguageService = Client;

impl Client {
    /// 按配置启动语言服务器子进程。
    ///
    /// 这只启动进程并启动读取线程；必须随后调用 [`initialize`](Self::initialize) 和
    /// [`initialized`](Self::initialized)。
    ///
    /// # Errors
    ///
    /// 程序不存在、无法创建管道或子进程启动失败时返回 [`io::Error`]。
    pub fn start(config: ServerConfig) -> io::Result<Self> {
        let mut command = Command::new(&config.program);
        if config.cangjie_defaults {
            command.args(["--enable-log=true", "-V", "--disableAutoImport"]);
            if let Some(path) = &config.log_path {
                command.arg(format!("--log-path={}", path.display()));
            }
        }
        command.args(config.args);
        for key in &config.env_remove {
            command.env_remove(key);
        }
        command.envs(config.env);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("语言服务器没有 stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("语言服务器没有 stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("语言服务器没有 stderr"))?;
        Self::from_pipes(
            child,
            stdin,
            stdout,
            stderr,
            InitializeState {
                root_uri: config.root_uri,
                capabilities: serde_json::to_value(&config.client_capabilities)
                    .map_err(io::Error::other)?,
                initialization_options: config.initialization_options.0,
                workspace_folders: config.workspace_folders,
                ..InitializeState::default()
            },
        )
    }

    /// 使用程序名与参数启动通用服务器。
    ///
    /// # Errors
    ///
    /// 无法启动子进程时返回 [`io::Error`]。
    pub fn spawn(
        program: impl AsRef<std::path::Path>,
        args: impl IntoIterator<Item = impl Into<String>>,
        envs: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
        root_uri: impl Into<String>,
        initialization_options: InitializeOptions,
        client_capabilities: crate::protocol::ClientCapabilities,
    ) -> io::Result<Self> {
        let mut config = ServerConfig::new(program)
            .args(args)
            .envs(envs)
            .root_uri(root_uri)
            .initialization_options(initialization_options)
            .client_capabilities(client_capabilities);
        config.cangjie_defaults = false;
        Self::start(config)
    }

    fn from_pipes(
        child: Child,
        stdin: ChildStdin,
        stdout: ChildStdout,
        stderr: ChildStderr,
        initialize_state: InitializeState,
    ) -> io::Result<Self> {
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let (pending, cancelled, closed, exited) = (
            Arc::new(Mutex::new(HashMap::<
                Id,
                std::sync::mpsc::Sender<Result<Value, JsonRpcError>>,
            >::new())),
            Arc::new(Mutex::new(HashMap::<Id, Arc<AtomicBool>>::new())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let reader_exited = Arc::clone(&exited);
        let reader_closed = Arc::clone(&closed);
        let reader_pending = Arc::clone(&pending);
        let stderr_events = event_sender.clone();
        let reader_events = event_sender.clone();
        let reader_thread = thread::Builder::new()
            .name("winc-lsp-reader".into())
            .spawn(move || {
                run_reader(stdout, reader_events, reader_pending);
                reader_exited.store(true, Ordering::Release);
                reader_closed.store(true, Ordering::Release);
                let _ = event_sender.send(Event::Closed(None));
            })?;
        let stderr_thread = thread::Builder::new()
            .name("winc-lsp-stderr".into())
            .spawn(move || {
                run_stderr(stderr, stderr_events);
            })?;
        Ok(Self {
            child: Arc::new(Mutex::new(child)),
            writer: Arc::new(Mutex::new(FramedWriter::new(BufWriter::new(stdin)))),
            events: Arc::new(Mutex::new(event_receiver)),
            pending,
            cancelled,
            next_id: AtomicI64::new(1),
            closed,
            reader_thread: Some(reader_thread),
            stderr_thread: Some(stderr_thread),
            initialized: Arc::new(AtomicBool::new(false)),
            shutdown_sent: Arc::new(AtomicBool::new(false)),
            exited,
            initialize_state,
        })
    }

    /// 移出公共事件接收器。
    ///
    /// 读取线程继续运行。若调用方先取走接收器，Client 的 [`recv_event`](Self::recv_event) /
    /// [`try_recv_event`](Self::try_recv_event) 将返回断开。
    pub fn take_events(&self) -> std::sync::mpsc::Receiver<Event> {
        let mut guard = self
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        std::mem::replace(&mut *guard, std::sync::mpsc::channel().1)
    }

    /// 阻塞接收下一个事件。
    ///
    /// # Errors
    ///
    /// 事件通道断开时返回错误。
    pub fn recv_event(&self) -> Result<Event, std::sync::mpsc::RecvError> {
        self.events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .recv()
    }

    /// 尝试非阻塞接收事件。
    ///
    /// # Errors
    ///
    /// 暂无事件或事件通道断开时返回错误。
    pub fn try_recv_event(&self) -> Result<Event, std::sync::mpsc::TryRecvError> {
        self.events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .try_recv()
    }

    /// 在指定时间内接收事件。
    ///
    /// # Errors
    ///
    /// 超时或事件通道断开时返回错误。
    pub fn recv_event_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Event, std::sync::mpsc::RecvTimeoutError> {
        self.events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .recv_timeout(timeout)
    }

    /// 发送 initialize 请求。
    ///
    /// 返回的句柄结果是服务器原始 initialize result JSON；可用
    /// [`serde_json::from_value::<InitializeResult>`](crate::transport::InitializeResult)
    /// 解析结构化字段。
    ///
    /// # Errors
    ///
    /// 已初始化、客户端已关闭或写入失败时返回错误。
    pub fn initialize(&mut self, timeout: Duration) -> Result<PendingRequest, io::Error> {
        if self.initialized.load(Ordering::Acquire) {
            return Err(io::Error::other("语言服务器已经初始化"));
        }
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "winc", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": self.initialize_state.root_uri,
            "capabilities": self.initialize_state.capabilities,
            "initializationOptions": self.initialize_state.initialization_options,
            "workspaceFolders": self.initialize_state.workspace_folders,
            "trace": "off"
        });
        let request = self.request("initialize", Some(params), timeout)?;
        self.initialized.store(true, Ordering::Release);
        Ok(request)
    }

    /// 发送 initialized 通知，并记录服务器信息。
    ///
    /// # Errors
    ///
    /// initialize 尚未发送、客户端已关闭或写入失败时返回错误。
    pub fn initialized(&mut self, server_info: Option<ServerInfo>) -> Result<(), io::Error> {
        if !self.initialized.load(Ordering::Acquire) {
            return Err(io::Error::other("initialize 尚未发送"));
        }
        self.notify("initialized", Some(json!({})))?;
        self.initialize_state.server_info = server_info;
        Ok(())
    }

    /// 打开文本文档。
    ///
    /// 仓颉的 languageId 必须精确传 `"Cangjie"`。版本应单调递增。
    ///
    /// # Errors
    ///
    /// 客户端已关闭或写入失败时返回 [`io::Error`]。
    pub fn did_open(
        &mut self,
        uri: impl Into<String>,
        language_id: impl Into<String>,
        version: i64,
        text: impl Into<String>,
    ) -> Result<(), io::Error> {
        self.notify(
            "textDocument/didOpen",
            Some(json!({
                "textDocument": TextDocumentItem::new(uri, language_id, version, text)
            })),
        )
    }

    /// 发送全量 didChange（`contentChanges` 只有一项且没有 range）。
    ///
    /// 协议层也支持增量同步；全量同步是 app 第一版的简单安全选择。
    ///
    /// # Errors
    ///
    /// 客户端已关闭或写入失败时返回 [`io::Error`]。
    pub fn did_change(
        &mut self,
        uri: impl Into<String>,
        version: i64,
        text: impl Into<String>,
    ) -> Result<(), io::Error> {
        self.did_change_events(uri, version, [TextDocumentContentChangeEvent::new(text)])
    }

    /// 使用调用方构造的变化列表发送 didChange。
    ///
    /// # Errors
    ///
    /// 客户端已关闭、变化列表为空或写入失败时返回 [`io::Error`]。
    pub fn did_change_events<I>(
        &mut self,
        uri: impl Into<String>,
        version: i64,
        changes: I,
    ) -> Result<(), io::Error>
    where
        I: IntoIterator<Item = TextDocumentContentChangeEvent>,
    {
        let changes: Vec<_> = changes.into_iter().collect();
        if changes.is_empty() {
            return Err(io::Error::other("didChange 至少需要一项变化"));
        }
        self.notify(
            "textDocument/didChange",
            Some(json!({
                "textDocument": VersionedTextDocumentIdentifier::new(uri, version),
                "contentChanges": changes
            })),
        )
    }

    /// 关闭文本文档。
    ///
    /// # Errors
    ///
    /// 客户端已关闭或写入失败时返回 [`io::Error`]。
    pub fn did_close(&mut self, uri: impl Into<String>) -> Result<(), io::Error> {
        self.notify(
            "textDocument/didClose",
            Some(json!({
                "textDocument": TextDocumentIdentifier::new(uri)
            })),
        )
    }

    /// 请求补全，结果由 [`PendingRequest`] 返回。
    ///
    /// # Errors
    ///
    /// 客户端已关闭或写入失败时返回 [`io::Error`]。
    pub fn completion(
        &mut self,
        uri: impl Into<String>,
        position: Position,
        timeout: Duration,
    ) -> Result<PendingRequest, io::Error> {
        self.request(
            "textDocument/completion",
            Some(json!({
                "textDocument": TextDocumentIdentifier::new(uri),
                "position": position
            })),
            timeout,
        )
    }

    /// 请求定义，结果可以是 `Location`、`Location[]` 或 `LocationLink[]`。
    ///
    /// # Errors
    ///
    /// 客户端已关闭或写入失败时返回 [`io::Error`]。
    pub fn definition(
        &mut self,
        uri: impl Into<String>,
        position: Position,
        timeout: Duration,
    ) -> Result<PendingRequest, io::Error> {
        self.request(
            "textDocument/definition",
            Some(json!({
                "textDocument": TextDocumentIdentifier::new(uri),
                "position": position
            })),
            timeout,
        )
    }

    /// 请求引用，结果通常是 `Location[]`。
    ///
    /// # Errors
    ///
    /// 客户端已关闭或写入失败时返回 [`io::Error`]。
    pub fn references(
        &mut self,
        uri: impl Into<String>,
        position: Position,
        include_declaration: bool,
        timeout: Duration,
    ) -> Result<PendingRequest, io::Error> {
        self.request(
            "textDocument/references",
            Some(json!({
                "textDocument": TextDocumentIdentifier::new(uri),
                "position": position,
                "context": { "includeDeclaration": include_declaration }
            })),
            timeout,
        )
    }

    /// 请求悬停结果。
    ///
    /// # Errors
    ///
    /// 客户端已关闭或写入失败时返回 [`io::Error`]。
    pub fn hover(
        &mut self,
        uri: impl Into<String>,
        position: Position,
        timeout: Duration,
    ) -> Result<PendingRequest, io::Error> {
        self.request(
            "textDocument/hover",
            Some(json!({
                "textDocument": TextDocumentIdentifier::new(uri),
                "position": position
            })),
            timeout,
        )
    }

    /// 发送任意 JSON-RPC 请求。
    ///
    /// 结果保留为原始 [`Value`]，可用于服务器扩展方法。`timeout` 仅决定调用方选择哪种等待 API，
    /// 不会自动删除 pending 项；响应到达后统一清理。
    ///
    /// # Errors
    ///
    /// 客户端已关闭、id 溢出或写入失败时返回 [`io::Error`]。
    pub fn request(
        &mut self,
        method: impl Into<String>,
        params: Option<Value>,
        _timeout: Duration,
    ) -> Result<PendingRequest, io::Error> {
        if self.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "语言服务器已关闭",
            ));
        }
        let id = Id::Number(self.next_id.fetch_add(1, Ordering::Relaxed));
        if self.next_id.load(Ordering::Relaxed) <= 0 {
            return Err(io::Error::other("JSON-RPC 请求 id 已耗尽"));
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(id.clone(), sender);
        self.cancelled
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(id.clone(), Arc::clone(&cancelled));
        let message = Request::new(id.clone(), method, params)
            .to_value()
            .map_err(io::Error::other)?;
        if let Err(error) = self.send_value(message) {
            self.pending
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&id);
            self.cancelled
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&id);
            return Err(error);
        }
        Ok(PendingRequest {
            id,
            receiver,
            cancelled,
        })
    }

    /// 发送任意 JSON-RPC 通知。
    ///
    /// # Errors
    ///
    /// 客户端已关闭、JSON 序列化或写入失败时返回 [`io::Error`]。
    pub fn notify(
        &mut self,
        method: impl Into<String>,
        params: Option<Value>,
    ) -> Result<(), io::Error> {
        if self.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "语言服务器已关闭",
            ));
        }
        let message = Notification::new(method, params)
            .to_value()
            .map_err(io::Error::other)?;
        self.send_value(message)
    }

    /// 回答服务器请求。
    ///
    /// # Errors
    ///
    /// 客户端已关闭或写入失败时返回 [`io::Error`]。
    pub fn respond(
        &mut self,
        id: Id,
        result: Result<Value, JsonRpcError>,
    ) -> Result<(), io::Error> {
        let value = match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
        };
        self.send_value(value)
    }

    /// 发送 shutdown 请求。
    ///
    /// 收到成功响应后调用 [`exit`](Self::exit)。调用方可以传入超时值并使用
    /// [`PendingRequest::recv_timeout`](PendingRequest::recv_timeout)；若超时，drop 或
    /// [`kill`](Self::kill) 仍会清理子进程。
    ///
    /// # Errors
    ///
    /// 已经关闭或写入失败时返回 [`io::Error`]。
    pub fn shutdown(&mut self, timeout: Duration) -> Result<PendingRequest, io::Error> {
        if self.shutdown_sent.swap(true, Ordering::AcqRel) {
            return Err(io::Error::other("shutdown 已发送"));
        }
        self.request("shutdown", Some(json!(null)), timeout)
    }

    /// 发送 exit 通知并按需要等待子进程结束。
    ///
    /// # Errors
    ///
    /// 写入失败时返回错误；进程未在超时内退出时返回
    /// [`io::ErrorKind::TimedOut`]，此时可调用 [`kill`](Self::kill)。
    pub fn exit(&mut self, timeout: Duration) -> Result<(), io::Error> {
        if self.closed.load(Ordering::Acquire) {
            return self.wait(timeout);
        }
        self.notify("exit", Some(json!(null)))?;
        self.closed.store(true, Ordering::Release);
        self.wait(timeout)
    }

    /// 强制终止服务器，并有限等待读取/stderr 线程退出。
    ///
    /// # Errors
    ///
    /// 当前实现不会返回错误；保留 `Result` 以便将来报告等待失败。
    pub fn kill(&mut self) -> io::Result<()> {
        self.closed.store(true, Ordering::Release);
        self.exited.store(true, Ordering::Release);
        let _ = self
            .child
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .kill();
        self.finish_threads(Duration::from_millis(500));
        Ok(())
    }

    /// 是否已经收到 exit 或观察到 EOF。
    pub fn is_exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    fn send_value(&self, message: Value) -> io::Result<()> {
        self.writer
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .write(&message)
    }

    fn wait(&mut self, timeout: Duration) -> io::Result<()> {
        let started = std::time::Instant::now();
        loop {
            let status = self
                .child
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .try_wait()?;
            if status.is_some() {
                self.closed.store(true, Ordering::Release);
                self.exited.store(true, Ordering::Release);
                self.finish_threads(Duration::from_millis(50));
                return Ok(());
            }
            if started.elapsed() >= timeout {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "语言服务器未在超时内退出",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn finish_threads(&mut self, timeout: Duration) {
        let started = std::time::Instant::now();
        while started.elapsed() < timeout {
            let reader_done = self
                .reader_thread
                .as_ref()
                .is_none_or(JoinHandle::is_finished);
            let stderr_done = self
                .stderr_thread
                .as_ref()
                .is_none_or(JoinHandle::is_finished);
            if reader_done && stderr_done {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        if let Some(thread) = self.reader_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.stderr_thread.take() {
            let _ = thread.join();
        }
    }

    fn finish_pending(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        pending.clear();
        self.cancelled
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }
}

/// initialize 结果的结构化视图。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// 服务器能力。
    pub capabilities: ServerCapabilities,
    /// 可选服务器信息。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_info: Option<ServerInfo>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        if !self.exited.load(Ordering::Acquire) {
            let already_exited = self
                .child
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .try_wait()
                .ok()
                .flatten()
                .is_some();
            if already_exited {
                self.exited.store(true, Ordering::Release);
            } else {
                let shutdown =
                    json!({"jsonrpc": "2.0", "id": -1, "method": "shutdown", "params": null});
                let _ = self.send_value(shutdown);
                let exit = json!({"jsonrpc": "2.0", "method": "exit", "params": null});
                let _ = self.send_value(exit);
                let started = std::time::Instant::now();
                while started.elapsed() < DROP_EXIT_TIMEOUT {
                    if self
                        .child
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .try_wait()
                        .ok()
                        .flatten()
                        .is_some()
                    {
                        self.exited.store(true, Ordering::Release);
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                if !self.exited.load(Ordering::Acquire) {
                    let _ = self
                        .child
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .kill();
                    let _ = self
                        .child
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .wait();
                }
            }
        }
        self.finish_pending();
        self.finish_threads(Duration::from_millis(100));
    }
}

// 这些字段由 start/spawn 在 from_pipes 前提取，单独保存以免 Reader 线程额外依赖配置。
// 为保持 Client 结构简单，它们放在独立 state 中。
#[derive(Default)]
struct InitializeState {
    root_uri: Option<String>,
    capabilities: Value,
    initialization_options: Value,
    workspace_folders: Vec<Value>,
    server_info: Option<ServerInfo>,
}

fn run_reader(
    mut reader: ChildStdout,
    events: std::sync::mpsc::Sender<Event>,
    pending: PendingMap,
) {
    let mut framed = FramedReader::new(&mut reader);
    loop {
        match framed.read() {
            Ok(value) => match Message::parse(value) {
                Ok(Message::Response(response)) => {
                    if let Some(sender) = pending
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .remove(&response.id)
                    {
                        let result = match response.payload {
                            crate::jsonrpc::ResponsePayload::Result(value) => Ok(value),
                            crate::jsonrpc::ResponsePayload::Error(error) => Err(error),
                        };
                        let _ = sender.send(result);
                    }
                }
                Ok(Message::ServerRequest(request)) => {
                    let _ = events.send(Event::ServerRequest(request));
                }
                Ok(Message::Notification(notification)) => {
                    if notification.method == "exit" {
                        if let Some(event) = Event::from_notification(notification) {
                            let _ = events.send(event);
                        }
                        break;
                    }
                    if let Some(event) = Event::from_notification(notification) {
                        let _ = events.send(event);
                    }
                }
                Err(error) => {
                    let _ = events.send(Event::ProtocolError(error));
                    break;
                }
            },
            Err(DecodeError::UnexpectedEof) => break,
            Err(error) => {
                let _ = events.send(Event::ProtocolError(error.to_string()));
                break;
            }
        }
    }
}

fn run_stderr(reader: ChildStderr, events: std::sync::mpsc::Sender<Event>) {
    let mut reader = std::io::BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                while line.ends_with(['\r', '\n']) {
                    line.pop();
                }
                if !line.is_empty() {
                    let _ = events.send(Event::Stderr(line.clone()));
                }
            }
        }
    }
}

// 简化 Event 中不需要额外公开类型的错误变体。
impl From<DecodeError> for Event {
    fn from(error: DecodeError) -> Self {
        Self::ProtocolError(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_server(script: &str) -> io::Result<Client> {
        let mut child = Command::new("python3")
            .arg("-u")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("缺少 stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("缺少 stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("缺少 stderr"))?;
        Client::from_pipes(child, stdin, stdout, stderr, InitializeState::default())
    }

    #[test]
    fn matches_out_of_order_responses_and_cancellation() {
        let script = r#"
import sys, json
def read():
    headers={}
    while True:
        line=sys.stdin.buffer.readline()
        if line in (b'\r\n', b'\n', b''): break
        k,v=line.decode().split(':',1); headers[k.lower()]=v.strip()
    return json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
def send(obj):
    body=json.dumps(obj).encode(); sys.stdout.buffer.write(b'Content-Length: '+str(len(body)).encode()+b'\r\n\r\n'+body); sys.stdout.buffer.flush()
first=read(); second=read()
send({'jsonrpc':'2.0','id':second['id'],'result':'second'})
send({'jsonrpc':'2.0','id':first['id'],'result':'first'})
read()
read()
"#;
        let mut client = test_server(script).unwrap();
        let first = client
            .request("first", None, Duration::from_secs(2))
            .unwrap();
        let second = client
            .request("second", None, Duration::from_secs(2))
            .unwrap();
        assert_eq!(first.recv_timeout(Duration::from_secs(2)).unwrap(), "first");
        assert_eq!(
            second.recv_timeout(Duration::from_secs(2)).unwrap(),
            "second"
        );
        let cancelled = client
            .request("cancel", None, Duration::from_secs(2))
            .unwrap();
        cancelled.cancel();
        assert_eq!(cancelled.recv(), Err(RequestError::Cancelled));
        client.exit(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn receives_server_exit_and_protocol_errors() {
        let script = r#"
import sys, json
def read():
    headers={}
    while True:
        line=sys.stdin.buffer.readline()
        if line in (b'\r\n', b'\n', b''): break
        k,v=line.decode().split(':',1); headers[k.lower()]=v.strip()
    return json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
read()
body=b'{bad'; sys.stdout.buffer.write(b'Content-Length: '+str(len(body)).encode()+b'\r\n\r\n'+body); sys.stdout.buffer.flush()
"#;
        let mut client = test_server(script).unwrap();
        client.notify("start", None).unwrap();
        assert!(matches!(
            client.recv_event_timeout(Duration::from_secs(2)).unwrap(),
            Event::ProtocolError(_)
        ));
        assert!(matches!(
            client.recv_event_timeout(Duration::from_secs(2)).unwrap(),
            Event::Closed(None)
        ));
        client.kill().unwrap();
    }

    #[test]
    fn receives_exit_notification() {
        let script = r#"
import sys, json
def read():
    headers={}
    while True:
        line=sys.stdin.buffer.readline()
        if line in (b'\r\n', b'\n', b''): break
        k,v=line.decode().split(':',1); headers[k.lower()]=v.strip()
    return json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
read(); body=b'{"jsonrpc":"2.0","method":"exit","params":null}'; sys.stdout.buffer.write(b'Content-Length: '+str(len(body)).encode()+b'\r\n\r\n'+body); sys.stdout.buffer.flush(); sys.stdin.buffer.read(1)
"#;
        let mut client = test_server(script).unwrap();
        client.notify("start", None).unwrap();
        assert!(matches!(
            client.recv_event_timeout(Duration::from_secs(2)).unwrap(),
            Event::Exit { .. }
        ));
        assert!(matches!(
            client.recv_event_timeout(Duration::from_secs(2)).unwrap(),
            Event::Closed(None)
        ));
        client.kill().unwrap();
    }

    #[test]
    fn start_initializes_and_closes() {
        let script = r#"
import sys, json, os
def read():
    headers={}
    while True:
        line=sys.stdin.buffer.readline()
        if line in (b'\r\n', b'\n', b''): break
        k,v=line.decode().split(':',1); headers[k.lower()]=v.strip()
    return json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
def send(obj):
    body=json.dumps(obj).encode(); sys.stdout.buffer.write(b'Content-Length: '+str(len(body)).encode()+b'\r\n\r\n'+body); sys.stdout.buffer.flush()
msg=read(); send({'jsonrpc':'2.0','id':msg['id'],'result':{'capabilities':{'textDocumentSync':2},'serverInfo':{'name':'test'}}})
msg=read()
if msg['method']=='initialized': pass
msg=read(); send({'jsonrpc':'2.0','id':msg['id'],'result':None})
msg=read()
if msg['method']!='exit': raise SystemExit(2)
"#;
        let mut client = test_server(script).unwrap();
        let result = client
            .initialize(Duration::from_secs(2))
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let result: InitializeResult = serde_json::from_value(result).unwrap();
        client.initialized(result.server_info).unwrap();
        let shutdown = client.shutdown(Duration::from_secs(2)).unwrap();
        assert_eq!(
            shutdown.recv_timeout(Duration::from_secs(2)).unwrap(),
            Value::Null
        );
        client.exit(Duration::from_secs(2)).unwrap();
    }
}
