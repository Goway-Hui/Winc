//! 每窗口独立的语言服务：启动仓颉 LSPServer、同步文本文档、接收诊断。
#![allow(
    clippy::disallowed_methods,
    reason = "the language server executable lives on the local machine; remote documents are skipped"
)]
//!
//! 协议层只通过 stdio 与服务器通信；reader 线程把事件批量交回窗口线程，界面在这里落地诊断并请求
//! 重绘。文档标识同时记录路径与 URI，以便把服务器的诊断映射回当前编辑器文档。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use winc_core::Lines;
use winc_editor::DocumentId;
use winc_i18n::t;
pub(crate) use winc_lsp::Event;
use winc_lsp::{Client, Position, ServerConfig};

use super::App;

const LANGUAGE_ID: &str = "Cangjie";
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_LSP_CHARS: usize = 2 << 20;

/// 语言服务的运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Status {
    /// 当前文档不需要语言服务。
    #[default]
    Off,
    /// 正在查找并启动 `LSPServer`。
    Starting,
    /// initialize 已完成。
    Ready,
    /// SDK 或 `LSPServer` 不存在。
    Unavailable,
    /// 子进程退出或初始化失败。
    Failed,
}

/// 诊断在编辑器中的范围；起止位置都是 char 偏移。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiagnosticRange {
    /// 起始 char 偏移。
    pub start: usize,
    /// 结束 char 偏移（不包含）。
    pub end: usize,
}

/// 一条已经映射到 winc 文本坐标的诊断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Diagnostic {
    /// 严重程度：1 错误、2 警告、3 信息、4 提示。
    pub severity: u8,
    /// 诊断正文。
    pub message: String,
    /// 诊断来源。
    pub source: Option<String>,
    /// 受影响的文本范围。
    pub range: DiagnosticRange,
    /// 诊断对应的服务器文档版本。
    pub version: i64,
}

/// 已经交给语言服务器的文档。
#[derive(Debug, Clone)]
struct OpenDocument {
    path: PathBuf,
    uri: String,
    version: i64,
    revision: u64,
}

/// 每个窗口独立的语言服务状态。
#[derive(Debug, Default)]
pub(crate) struct LanguageServiceState {
    pub(crate) client: Option<Client>,
    root: Option<PathBuf>,
    single_file: Option<PathBuf>,
    open: HashMap<DocumentId, OpenDocument>,
    pub(crate) diagnostics: HashMap<DocumentId, Vec<Diagnostic>>,
    pub(crate) status: Status,
    pub(crate) last_error: Option<String>,
    generation: u64,
    initialized: bool,
    requests: HashSet<u64>,
    revisions: HashMap<DocumentId, u64>,
    next_request: u64,
    /// 仅测试使用：启动时的程序与环境，避免改动进程全局环境。
    #[cfg(test)]
    pub(crate) test_server: Option<ServerConfig>,
}

impl LanguageServiceState {
    /// 当前状态栏文案。
    pub(crate) fn status_text(&self) -> &'static str {
        match self.status {
            Status::Off | Status::Unavailable => t!("cangjie-lsp-unavailable"),
            Status::Starting => t!("cangjie-lsp-starting"),
            Status::Ready => t!("cangjie-lsp-connected"),
            Status::Failed => t!("cangjie-lsp-failed"),
        }
    }

    /// 当前文件的诊断数量。
    pub(crate) fn problem_count(&self, id: Option<DocumentId>) -> usize {
        id.and_then(|id| self.diagnostics.get(&id))
            .map_or(0, Vec::len)
    }

    /// 当前状态栏是否应显示。
    pub(crate) fn shown(&self) -> bool {
        self.status != Status::Off || self.client.is_some()
    }
    /// 文档、路径或 revision 集合是否变化；帧入口只在变化时同步文本。
    pub(crate) fn documents_changed(&self, documents: &[winc_editor::Document]) -> bool {
        if self.open.len()
            != documents
                .iter()
                .filter(|doc| doc.highlighter.language().id == "cangjie")
                .count()
        {
            return true;
        }
        documents.iter().any(|doc| {
            if doc.highlighter.language().id != "cangjie" {
                return self.revisions.contains_key(&doc.id);
            }
            match (self.open.get(&doc.id), self.revisions.get(&doc.id)) {
                (Some(open), Some(&revision)) => {
                    doc.path.as_deref().is_none_or(|path| open.path != path)
                        || revision != doc.revision
                }
                _ => true,
            }
        })
    }

    /// 记录本帧采用的文档 revision，供下一帧判断是否真的有文本变化。
    pub(crate) fn remember_revisions(&mut self, documents: &[winc_editor::Document]) {
        self.revisions = documents
            .iter()
            .filter(|doc| doc.highlighter.language().id == "cangjie")
            .map(|doc| (doc.id, doc.revision))
            .collect();
    }

    /// 切换 root 或退出时关闭旧 client，使迟到的 reader 事件失效。
    pub(crate) fn stop(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.initialized = false;
        self.requests.clear();
        self.open.clear();
        self.diagnostics.clear();
        self.revisions.clear();
        self.root = None;
        self.single_file = None;
        self.status = Status::Off;
        self.last_error = None;
        if let Some(mut client) = self.client.take() {
            let pending = client.shutdown(SHUTDOWN_TIMEOUT);
            if let Ok(pending) = pending {
                let _ = pending.recv_timeout(SHUTDOWN_TIMEOUT);
            }
            let _ = client.exit(SHUTDOWN_TIMEOUT);
            if client.is_exited() {
                return;
            }
            let _ = client.kill();
        }
    }

    /// 返回当前启动代次；reader 事件必须与它相同。
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    /// 记录一个仍在等待的请求；响应、错误或服务器退出时清除。
    pub(crate) fn track_request(&mut self, id: &winc_lsp::Id) {
        if let winc_lsp::Id::Number(id) = id {
            self.next_request = self.next_request.max((*id as u64).saturating_add(1));
            self.requests.insert(*id as u64);
        }
    }

    fn forget_request(&mut self, id: &winc_lsp::Id) {
        if let winc_lsp::Id::Number(id) = id {
            self.requests.remove(&(*id as u64));
        }
    }

    fn fail(&mut self, reason: impl Into<String>) {
        self.status = Status::Failed;
        self.last_error = Some(reason.into());
        self.initialized = false;
        self.requests.clear();
        if let Some(mut client) = self.client.take() {
            let pending = client.shutdown(SHUTDOWN_TIMEOUT);
            if let Ok(pending) = pending {
                let _ = pending.recv_timeout(SHUTDOWN_TIMEOUT);
            }
            let _ = client.exit(SHUTDOWN_TIMEOUT);
            if client.is_exited() {
                return;
            }
            let _ = client.kill();
        }
    }

    /// reader 线程只调用这个非阻塞发送；窗口关闭后代理失效，线程随即退出。
    pub(crate) fn forward_events(
        events: mpsc::Receiver<Event>,
        proxy: crate::winc::WindowProxy,
        generation: u64,
    ) {
        std::thread::Builder::new()
            .name("winc-lsp-events".into())
            .spawn(move || {
                let mut batch = Vec::new();
                while let Ok(event) = events.recv() {
                    batch.push(event);
                    while batch.len() < 64 {
                        match events.try_recv() {
                            Ok(event) => batch.push(event),
                            Err(_) => break,
                        }
                    }
                    if proxy
                        .send_event(crate::app::UserEvent::LanguageService(
                            generation,
                            std::mem::take(&mut batch),
                        ))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("启动语言服务事件线程");
    }
}

impl App {
    /// root 或窗口恢复后确保语言服务与当前 root 一致；没有仓颉文档时不启动。
    pub(in crate::app) fn ensure_lsp_started(&mut self) {
        if self.config.cangjie_lsp_enabled == Some(false) {
            if self.lsp.client.is_some() || self.lsp.status != Status::Off {
                self.lsp.stop();
            }
            return;
        }
        let cangjie: Vec<DocumentId> = self
            .editor
            .documents
            .iter()
            .filter(|doc| doc.highlighter.language().id == "cangjie")
            .map(|doc| doc.id)
            .collect();
        let root = self
            .editor
            .root()
            .filter(|_| self.lsp_root_supported())
            .map(Path::to_path_buf);
        let single = self
            .editor
            .active()
            .filter(|doc| doc.highlighter.language().id == "cangjie")
            .and_then(|doc| doc.path.clone())
            .filter(|path| self.lsp_path_supported(path));
        let target = if cangjie.is_empty() {
            None
        } else {
            root.or(single)
        };
        if target.is_none() && cangjie.is_empty() {
            if self.lsp.client.is_some()
                || matches!(self.lsp.status, Status::Starting | Status::Ready)
            {
                self.lsp.stop();
            }
            return;
        }
        if self.lsp.status == Status::Ready && self.lsp.root == target {
            self.sync_lsp_documents();
            return;
        }
        if self.lsp.status == Status::Starting && self.lsp.root == target {
            return;
        }
        self.restart_lsp(target);
    }

    /// 点击状态栏重试：按当前 root 或活动仓颉文档重新启动服务。
    pub(in crate::app) fn retry_lsp(&mut self) {
        let root = self.editor.root().filter(|_| self.lsp_root_supported());
        let single = self
            .editor
            .active()
            .filter(|doc| doc.highlighter.language().id == "cangjie")
            .and_then(|doc| doc.path.clone())
            .filter(|path| self.lsp_path_supported(path));
        self.restart_lsp(root.map(Path::to_path_buf).or(single));
        if self.lsp.status != Status::Unavailable {
            self.flash(t!("cangjie-lsp-restart"));
        }
    }

    /// 点击状态栏或命令面板中的「重启仓颉语言服务」。
    pub(in crate::app) fn restart_lsp(&mut self, root: Option<PathBuf>) {
        self.intelligence.reset();
        self.lsp.stop();
        let Some(root) = root else {
            self.lsp.status = Status::Unavailable;
            self.lsp.last_error = Some(t!("cangjie-lsp-unavailable-detail").to_owned());
            self.request_redraw();
            return;
        };
        self.lsp.status = Status::Starting;
        self.lsp.root = Some(root.clone());
        self.request_redraw();

        let program = {
            #[cfg(test)]
            if let Some(config) = self.lsp.test_server.clone() {
                Some(config)
            } else {
                self.lsp_server_config(&root)
            }
            #[cfg(not(test))]
            self.lsp_server_config(&root)
        };
        let Some(mut config) = program else {
            self.lsp.status = Status::Unavailable;
            self.lsp.last_error = Some(t!("cangjie-lsp-unavailable-detail").to_owned());
            return;
        };
        config.root_uri = Some(file_uri(&root));
        let single_file = self
            .editor
            .active()
            .filter(|doc| doc.highlighter.language().id == "cangjie")
            .and_then(|doc| doc.path.clone())
            .filter(|path| self.lsp_path_supported(path));
        if let Some(path) = &single_file {
            config.initialization_options = json!({ "singleFilePath": file_uri(path) }).into();
        }
        if let Some(logs) = self.lsp_log_dir() {
            config.log_path = Some(logs);
        }
        #[cfg(target_os = "linux")]
        {
            let dirs = sdk_library_dirs(&config.program);
            if !dirs.is_empty() {
                let value = std::env::join_paths(
                    dirs.into_iter().chain(
                        std::env::var_os("LD_LIBRARY_PATH")
                            .as_deref()
                            .into_iter()
                            .flat_map(std::env::split_paths),
                    ),
                )
                .ok()
                .and_then(|value| value.into_string().ok());
                if let Some(value) = value {
                    config = config.env("LD_LIBRARY_PATH", value);
                }
            }
        }

        let mut client = match Client::start(config) {
            Ok(client) => client,
            Err(error) => {
                self.lsp.status = Status::Unavailable;
                self.lsp.last_error = Some(error.to_string());
                return;
            }
        };
        let generation = self.lsp.generation();
        let Some(proxy) = self.link.proxy.clone() else {
            self.initialize_lsp_client(&mut client);
            self.lsp.client = Some(client);
            self.sync_lsp_documents();
            return;
        };
        let events = client.take_events();
        LanguageServiceState::forward_events(events, proxy, generation);
        self.initialize_lsp_client(&mut client);
        self.lsp.client = Some(client);
        self.sync_lsp_documents();
    }

    fn initialize_lsp_client(&mut self, client: &mut Client) {
        let pending = match client.initialize(INITIALIZE_TIMEOUT) {
            Ok(pending) => pending,
            Err(error) => {
                self.lsp.fail(error.to_string());
                return;
            }
        };
        let pending_id = pending.id().clone();
        self.lsp.track_request(pending.id());
        let result = pending.recv_timeout(INITIALIZE_TIMEOUT);
        self.lsp.forget_request(&pending_id);
        match result {
            Ok(value) => {
                let server_info = serde_json::from_value::<winc_lsp::ServerInfo>(
                    value.get("serverInfo").cloned().unwrap_or(Value::Null),
                )
                .ok();
                if let Err(error) = client.initialized(server_info) {
                    self.lsp.fail(error.to_string());
                    return;
                }
                self.lsp.initialized = true;
                self.lsp.status = Status::Ready;
                self.lsp.last_error = None;
            }
            Err(error) => self.lsp.fail(error.to_string()),
        }
    }
    fn lsp_server_config(&self, root: &Path) -> Option<ServerConfig> {
        if cfg!(test) {
            return None;
        }
        let sdk = crate::app::cangjie::sdk_root(self.config.cangjie_sdk_path.as_deref());
        let program = find_lsp_server(
            self.config.cangjie_sdk_path.as_deref(),
            self.config.cangjie_lsp_path.as_deref(),
        )?;
        let mut config = ServerConfig::new(program).root_uri(file_uri(root));
        if let Some(sdk) = &sdk {
            let mut paths = vec![sdk.join("bin"), sdk.join("tools/bin")];
            if let Some(path) = std::env::var_os("PATH") {
                paths.extend(std::env::split_paths(&path));
            }
            if let Ok(path) = std::env::join_paths(paths) {
                config = config.env("PATH", path.to_string_lossy());
            }
            config = config.env("CANGJIE_HOME", sdk.to_string_lossy());
        }
        if let Some(logs) = self.lsp_log_dir() {
            let _ = std::fs::create_dir_all(&logs);
            config = config.log_path(logs);
        }
        Some(config)
    }

    fn lsp_log_dir(&self) -> Option<PathBuf> {
        if cfg!(test) {
            return None;
        }
        winc_config::config_path()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .map(|dir| dir.join("cangjie-lsp"))
    }

    fn lsp_path_supported(&self, path: &Path) -> bool {
        !winc_backend::is_remote(path)
            && winc_editor::wsl::locate(path).is_none()
            && !path.as_os_str().as_encoded_bytes().is_empty()
    }

    fn lsp_root_supported(&self) -> bool {
        !self.editor.root().is_some_and(winc_backend::is_remote)
            && !self
                .editor
                .root()
                .is_some_and(|root| winc_editor::wsl::locate(root).is_some())
    }

    /// 同步当前所有仓颉文档：新打开的 didOpen，revision 改变的 didChange，已关闭的 didClose。
    pub(in crate::app) fn sync_lsp_documents(&mut self) {
        if !self.lsp.initialized {
            return;
        }
        let mut wanted = HashSet::new();
        let mut docs: Vec<(DocumentId, PathBuf, u64, String)> = Vec::new();
        let mut close = Vec::new();
        for doc in &self.editor.documents {
            if doc.highlighter.language().id != "cangjie" {
                continue;
            }
            let Some(path) = &doc.path else {
                continue;
            };
            let eligible = doc.shows_text()
                && !doc.is_pending()
                && !doc.is_large()
                && self.lsp_path_supported(path)
                && doc.text().end_pos() <= MAX_LSP_CHARS;
            if eligible {
                let text = doc.text().substring(0, doc.text().end_pos());
                docs.push((doc.id, path.clone(), doc.revision, text));
            } else if let Some(open) = self.lsp.open.remove(&doc.id) {
                close.push(open.uri);
                self.lsp.diagnostics.remove(&doc.id);
            }
        }
        let mut changes = Vec::new();
        for (id, path, revision, text) in docs {
            wanted.insert(id);
            let uri = file_uri(&path);
            match self.lsp.open.get(&id) {
                Some(open) if open.path == path && open.revision == revision => {}
                Some(open) if open.path == path => {
                    if open.revision != revision {
                        self.lsp.diagnostics.remove(&id);
                    }
                    let version = revision.min(i64::MAX as u64) as i64;
                    changes.push((SyncChange::Change, uri.clone(), version, text));
                    self.lsp.open.insert(
                        id,
                        OpenDocument {
                            path,
                            uri,
                            version,
                            revision,
                        },
                    );
                }
                Some(_) | None => {
                    if let Some(old) = self.lsp.open.remove(&id) {
                        close.push(old.uri);
                    }
                    let version = revision.min(i64::MAX as u64) as i64;
                    changes.push((SyncChange::Open, uri.clone(), version, text));
                    self.lsp.open.insert(
                        id,
                        OpenDocument {
                            path,
                            uri,
                            version,
                            revision,
                        },
                    );
                }
            }
        }
        let stale: Vec<DocumentId> = self
            .lsp
            .open
            .keys()
            .copied()
            .filter(|id| !wanted.contains(id))
            .collect();
        for id in stale {
            if let Some(open) = self.lsp.open.remove(&id) {
                close.push(open.uri);
                self.lsp.diagnostics.remove(&id);
            }
        }
        let Some(client) = self.lsp.client.as_mut() else {
            return;
        };
        for uri in close {
            let _ = client.did_close(uri);
        }
        for (kind, uri, version, text) in changes {
            let result = match kind {
                SyncChange::Open => client.did_open(uri, LANGUAGE_ID, version, text),
                SyncChange::Change => client.did_change(uri, version, text),
            };
            if let Err(error) = result {
                self.lsp.last_error = Some(error.to_string());
            }
        }
        self.lsp.remember_revisions(&self.editor.documents);
    }

    /// 应用 reader 送回的一批事件；启动代次不同说明 root 已切换，整批丢弃。
    pub(in crate::app) fn handle_lsp_events(&mut self, generation: u64, events: Vec<Event>) {
        if generation != self.lsp.generation() {
            return;
        }
        let mut changed = false;
        for event in events {
            match event {
                Event::Diagnostics {
                    uri,
                    version,
                    diagnostics,
                } => {
                    let Some((id, open)) = self
                        .lsp
                        .open
                        .iter()
                        .find(|(_, open)| open.uri == uri)
                        .map(|(&id, open)| (id, open.clone()))
                    else {
                        continue;
                    };
                    if version.is_some_and(|version| version != open.version) {
                        continue;
                    }
                    let Some(doc) = self.editor.document(id) else {
                        continue;
                    };
                    let text = doc.text().substring(0, doc.text().end_pos());
                    let converted: Vec<Diagnostic> = diagnostics
                        .into_iter()
                        .filter_map(|diagnostic| {
                            let start = Position::to_char_offset(diagnostic.range.start, &text)?;
                            let end = Position::to_char_offset(diagnostic.range.end, &text)?;
                            Some(Diagnostic {
                                severity: diagnostic.severity.unwrap_or(1),
                                source: diagnostic.source,
                                message: diagnostic.message,
                                range: DiagnosticRange {
                                    start: start.min(end),
                                    end: start.max(end),
                                },
                                version: open.version,
                            })
                        })
                        .collect();
                    if converted.is_empty() {
                        self.lsp.diagnostics.remove(&id);
                    } else {
                        self.lsp.diagnostics.insert(id, converted);
                    }
                    changed = true;
                }
                Event::Exit { .. } | Event::Closed(_) => {
                    if self.lsp.client.is_some() {
                        self.lsp.fail("language server exited".to_owned());
                        changed = true;
                    }
                    break;
                }
                Event::LogMessage { message, .. } => {
                    self.lsp.last_error = Some(message);
                }
                Event::ProtocolError(error) => {
                    self.lsp.fail(error);
                    changed = true;
                    break;
                }
                Event::ServerRequest(_) | Event::Stderr(_) | Event::Notification { .. } => {}
            }
        }
        if changed {
            self.request_redraw();
        }
    }

    /// 关闭窗口或切换 root 时调用；不保留旧诊断。
    pub(in crate::app) fn close_lsp(&mut self) {
        self.lsp.stop();
    }
}

#[derive(Clone, Copy)]
enum SyncChange {
    Open,
    Change,
}

/// 把本地绝对路径编码为 LSP file URI；非 UTF-8 字节按 `OsStr` 原始字节百分号编码。
pub(crate) fn file_uri(path: &Path) -> String {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let text = path.as_os_str().as_encoded_bytes();
    let mut encoded = String::with_capacity(text.len() + 8);
    for &byte in text {
        if byte.is_ascii_alphanumeric() || b"-_.~/".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    if cfg!(windows) {
        format!("file:///{encoded}")
    } else {
        format!("file://{encoded}")
    }
}

/// 按 `PATH`、`CANGJIE_HOME`、`~/.cjv` 的顺序查找 `LSPServer`。
fn find_lsp_server(configured_sdk: Option<&str>, configured_lsp: Option<&str>) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "LSPServer.exe"
    } else {
        "LSPServer"
    };
    if let Some(path) = configured_lsp
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
    {
        return executable(&path);
    }
    if let Some(root) = crate::app::cangjie::sdk_root(configured_sdk) {
        for path in [root.join("tools/bin").join(exe), root.join("bin").join(exe)] {
            if let Some(path) = executable(&path) {
                return Some(path);
            }
        }
    }
    if let Some(path) = std::env::var_os("PATH")
        .and_then(|paths| std::env::split_paths(&paths).find_map(|dir| executable(&dir.join(exe))))
    {
        return Some(path);
    }
    if let Some(home) = std::env::var_os("CANGJIE_HOME").filter(|value| !value.is_empty()) {
        let home = PathBuf::from(home);
        for path in [home.join("tools/bin").join(exe), home.join("bin").join(exe)] {
            if let Some(path) = executable(&path) {
                return Some(path);
            }
        }
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let cjv = PathBuf::from(home).join(".cjv");
    if let Some(path) = executable(&cjv.join("bin").join(exe)) {
        return Some(path);
    }
    let toolchains = std::fs::read_dir(cjv.join("toolchains")).ok()?;
    for entry in toolchains.flatten() {
        for path in [
            entry.path().join("tools/bin").join(exe),
            entry.path().join("bin").join(exe),
        ] {
            if let Some(path) = executable(&path) {
                return Some(path);
            }
        }
    }
    None
}

fn executable(path: &Path) -> Option<PathBuf> {
    path.is_file().then(|| path.to_path_buf())
}

/// Linux 上把 SDK 自带的 libcangjie-lsp.so 所在目录加入子进程搜索路径。
#[cfg(target_os = "linux")]
fn sdk_library_dirs(lsp: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(root) = lsp.ancestors().nth(3) {
        roots.push(root.to_path_buf());
    }
    if let Some(home) = std::env::var_os("CANGJIE_HOME").filter(|value| !value.is_empty()) {
        roots.push(PathBuf::from(home));
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let cjv = PathBuf::from(home).join(".cjv");
        roots.push(cjv.clone());
        if let Ok(entries) = std::fs::read_dir(cjv.join("toolchains")) {
            roots.extend(entries.flatten().map(|entry| entry.path()));
        }
    }
    roots
        .into_iter()
        .flat_map(|root| {
            [
                root.join("lib"),
                root.join("tools/lib"),
                root.join("tools/lib/third_party/llvm/lib"),
                root.join("third_party/llvm/lib"),
            ]
        })
        .filter(|path| path.is_dir())
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn sdk_library_dirs(_lsp: &Path) -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(test)]
mod tests;
