#![allow(
    clippy::disallowed_methods,
    reason = "debug adapters, SDK files and build artifacts are local; remote projects are rejected"
)]

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use winc_dap::{
    Event as DapEvent, InitializeRequest, SetBreakpointsRequest, Source, SourceBreakpoint,
    TcpConfig,
};
use winc_editor::DocumentId;

use crate::app::{App, NoticeKind, UserEvent};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
use winc_core::Lines;
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OUTPUT_CHARS: usize = 200_000;

/// Monotonic ticket carried by every asynchronous debug result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Generation(pub u64);

impl Generation {
    fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// One persisted breakpoint. Lines and columns are zero-based char offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Breakpoint {
    pub document: DocumentId,
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
    pub enabled: bool,
    pub verified: bool,
    pub message: Option<String>,
}

/// Debugger execution state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DebugStatus {
    #[default]
    Idle,
    Starting,
    Running,
    Stopped,
    Terminated,
    Failed,
}

/// Visible call-stack frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StackFrame {
    pub id: i64,
    pub name: String,
    pub path: Option<PathBuf>,
    pub line: Option<usize>,
    pub column: Option<usize>,
}

/// Visible variable node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Variable {
    pub name: String,
    pub value: String,
    pub kind: Option<String>,
    pub reference: i64,
    pub expanded: bool,
    pub children: Vec<Variable>,
}

/// Visible lexical scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Scope {
    pub reference: i64,
    pub name: String,
    pub variables: Vec<Variable>,
    pub loaded: bool,
}

/// Debug panel controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DebugControl {
    Start,
    Stop,
    Continue,
    Next,
    StepIn,
    StepOut,
    Close,
}

impl DebugControl {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Start => winc_i18n::t!("debug-start"),
            Self::Stop => winc_i18n::t!("debug-stop"),
            Self::Continue => winc_i18n::t!("debug-continue"),
            Self::Next => winc_i18n::t!("debug-next"),
            Self::StepIn => winc_i18n::t!("debug-step-in"),
            Self::StepOut => winc_i18n::t!("debug-step-out"),
            Self::Close => winc_i18n::t!("debug-close"),
        }
    }
}

/// One path through the variable tree; kept Copy so hover and hit enums stay Copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) struct DebugPath {
    indices: [u32; 8],
    len: u8,
}

impl DebugPath {
    pub(crate) fn new(path: &[usize]) -> Self {
        let mut value = Self::default();
        for &index in path.iter().take(value.indices.len()) {
            value.indices[usize::from(value.len)] = index as u32;
            value.len += 1;
        }
        value
    }
    pub(crate) fn get(self, index: usize) -> Option<usize> {
        (index < usize::from(self.len)).then(|| self.indices[index] as usize)
    }

    pub(crate) fn with_child(self, child: usize) -> Self {
        let mut value = self;
        if usize::from(value.len) < value.indices.len() {
            value.indices[usize::from(value.len)] = child as u32;
            value.len += 1;
        }
        value
    }
}

/// Per-frame geometry and selection.
#[derive(Debug, Clone, Default)]
pub(crate) struct DebugUi {
    pub open: bool,
    pub control_hits: Vec<(winc_render::Rect, DebugControl)>,
    pub stack_hits: Vec<(winc_render::Rect, usize)>,
    pub variable_hits: Vec<(winc_render::Rect, DebugPath)>,
    pub output_scroll: usize,
    pub selected_frame: usize,
    pub selected_scope: usize,
}

#[derive(Debug, Clone)]
enum Pending {
    Initialize,
    Launch,
    ConfigurationDone,
    Breakpoints(PathBuf),
    Threads,
    Stack(i64),
    Scopes(i64),
    Variables(i64),
    Control,
    Disconnect,
}

/// App-owned debugger state. The worker owns the DAP client and process.
#[derive(Default)]
pub(crate) struct DebugState {
    pub status: DebugStatus,
    pub generation: Generation,
    pub program: Option<PathBuf>,
    pub root: Option<PathBuf>,
    pub adapter: Option<PathBuf>,
    pub breakpoints: Vec<Breakpoint>,
    pub threads: Vec<(i64, String)>,
    pub active_thread: Option<i64>,
    pub stack: Vec<StackFrame>,
    pub scopes: Vec<Scope>,
    pub output: String,
    pub output_truncated: bool,
    pub error: Option<String>,
    pub ui: DebugUi,
    reader: Option<mpsc::Receiver<ReaderEvent>>,
    writer: Option<mpsc::Sender<ReaderCommand>>,
    pending: Option<(u64, Generation, Pending)>,
    next_request: u64,
    launch_deadline: Option<Instant>,
    pending_breakpoint_paths: Vec<PathBuf>,
    configuration_done: bool,
}

#[derive(Debug)]
struct ReaderCommand {
    generation: Generation,
    id: u64,
    method: String,
    params: Value,
    disconnect: bool,
}

/// Messages from the DAP worker, tagged with their session generation.
#[derive(Debug)]
pub(crate) enum ReaderEvent {
    Connected(Generation, Result<(), String>),
    Response(Generation, u64, Result<Value, String>),
    Event(Generation, DapEvent),
    Closed(Generation),
}

/// Approved launch specification.
#[derive(Debug, Clone)]
pub(crate) struct DebugLaunch {
    pub config: winc_config::RunConfig,
    pub program: PathBuf,
    pub adapter: PathBuf,
    pub root: PathBuf,
}

impl App {
    pub(crate) fn debug_shown(&self) -> bool {
        self.debug.ui.open && self.layout.terminal.h > 0.0
    }
    pub(crate) fn debug_active(&self) -> bool {
        !matches!(
            self.debug.status,
            DebugStatus::Idle | DebugStatus::Terminated | DebugStatus::Failed
        )
    }
    pub(crate) fn debug_can_step(&self) -> bool {
        self.debug.status == DebugStatus::Stopped && self.debug.active_thread.is_some()
    }

    pub(crate) fn debug_status_text(&self) -> String {
        match self.debug.status {
            DebugStatus::Idle => winc_i18n::t!("debug-status-idle").to_owned(),
            DebugStatus::Starting => winc_i18n::t!("debug-status-starting").to_owned(),
            DebugStatus::Running => winc_i18n::t!("debug-status-running").to_owned(),
            DebugStatus::Stopped => winc_i18n::t!("debug-status-stopped").to_owned(),
            DebugStatus::Terminated => winc_i18n::t!("debug-status-terminated").to_owned(),
            DebugStatus::Failed => winc_i18n::t!("debug-status-failed").to_owned(),
        }
    }

    pub(in crate::app) fn open_debug_panel(&mut self) {
        self.debug.ui.open = true;
        if !self.terminal.open {
            self.terminal.open = true;
            if self.terminal.height <= 0.0 {
                self.terminal.height = crate::app::terminal::DEFAULT_HEIGHT;
            }
        }
        self.focus = crate::app::Focus::DebugPanel;
        self.relayout();
        self.request_redraw();
    }

    pub(in crate::app) fn close_debug_panel(&mut self) {
        self.debug.ui.open = false;
        if self.focus == crate::app::Focus::DebugPanel {
            self.focus = crate::app::Focus::Editor;
        }
        self.relayout();
        self.request_redraw();
    }

    /// Starts debugging only after the existing `RunConfig` approval chain accepts the command.
    pub(in crate::app) fn start_debug(&mut self) {
        if self.debug_active() {
            return;
        }
        if self.remote_has_no_run_configs() {
            return;
        }
        let Some(root) = self.editor.root().map(Path::to_path_buf) else {
            self.notify(NoticeKind::Warning, winc_i18n::t!("debug-no-project"));
            return;
        };
        if winc_backend::is_remote(&root) || winc_editor::wsl::locate(&root).is_some() {
            self.notify(
                NoticeKind::Warning,
                winc_i18n::t!("debug-remote-unsupported"),
            );
            return;
        }
        if !root.join("cjpm.toml").is_file() {
            self.notify(
                NoticeKind::Warning,
                winc_i18n::t!("debug-not-cangjie-project"),
            );
            return;
        }
        let Some(adapter) = debug_adapter_path(
            &root,
            self.config.debug_adapter.as_deref(),
            self.config.cangjie_sdk_path.as_deref(),
        ) else {
            self.notify(NoticeKind::Error, winc_i18n::t!("debug-adapter-missing"));
            return;
        };
        let Some(cjpm) = crate::app::cangjie::find_cjpm(self.config.cangjie_sdk_path.as_deref())
        else {
            self.notify(NoticeKind::Error, winc_i18n::t!("debug-cjpm-missing"));
            return;
        };
        let explicit = self
            .config
            .debug_program
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let (command, args, program) = if let Some(program) = explicit {
            let path = resolve_program(&root, program);
            let command = path.to_string_lossy().into_owned();
            (command, Vec::new(), path)
        } else {
            (
                cjpm.to_string_lossy().into_owned(),
                vec!["build".to_owned(), "-g".to_owned()],
                built_program(&root),
            )
        };
        let config = winc_config::RunConfig {
            name: "Cangjie Debug".to_owned(),
            command,
            args,
            cwd: Some(root.to_string_lossy().into_owned()),
            env: Vec::new(),
        };
        let key = crate::app::terminal::run::debug_trust_key(&root, &config);
        let approval = config.approval();
        if self.state.command_approved(&key, &approval) {
            self.begin_debug(DebugLaunch {
                config,
                program,
                adapter,
                root,
            });
            return;
        }
        if self.window.handle.is_none() {
            self.notify(
                NoticeKind::Warning,
                winc_i18n::t!("debug-approval-required"),
            );
            return;
        }
        let message = winc_i18n::t!("terminal-run-confirm", name = config.name.as_str());
        let command_line = format!("{} {}", config.command, config.args.join(" "));
        let detail = format!(
            "{}\n\n{}\n{}\n{}",
            winc_i18n::t!("debug-confirm-detail"),
            winc_i18n::t!("terminal-run-confirm-command", command = command_line),
            winc_i18n::t!("terminal-run-confirm-cwd", cwd = root.to_string_lossy()),
            winc_i18n::t!("debug-adapter", path = adapter.to_string_lossy())
        );
        let buttons = vec![
            crate::app::AlertButton::new(winc_i18n::t!("debug-start")),
            crate::app::AlertButton::cancel(winc_i18n::t!("common-cancel")),
        ];
        let launch = DebugLaunch {
            config: config.clone(),
            program,
            adapter,
            root,
        };
        self.show_alert(&message, &detail, buttons, None, move |index, _| {
            (index == 0).then(|| UserEvent::DebugApproved(Box::new(launch.clone())))
        });
    }

    pub(in crate::app) fn approved_debug(&mut self, launch: DebugLaunch) {
        if let Some(root) = self.editor.root() {
            let key = crate::app::terminal::run::debug_trust_key(root, &launch.config);
            let approval = launch.config.approval();
            self.update_state(move |state| state.approve_command_only(&key, &approval));
        }
        self.begin_debug(launch);
    }

    fn begin_debug(&mut self, launch: DebugLaunch) {
        self.stop_debug_session();
        self.open_debug_panel();
        self.debug.generation = self.debug.generation.next();
        self.debug.status = DebugStatus::Starting;
        self.debug.root = Some(launch.root.clone());
        self.debug.program = Some(launch.program.clone());
        self.debug.adapter = Some(launch.adapter.clone());
        self.debug.error = None;
        self.debug.stack.clear();
        self.debug.scopes.clear();
        self.debug.threads.clear();
        self.debug.active_thread = None;
        self.debug.output.clear();
        self.debug.output_truncated = false;
        self.debug.configuration_done = false;
        self.debug.pending_breakpoint_paths.clear();
        self.debug.launch_deadline = Some(Instant::now() + LAUNCH_TIMEOUT);
        let generation = self.debug.generation;
        let (event_tx, event_rx) = mpsc::channel::<ReaderEvent>();
        let (command_tx, command_rx) = mpsc::channel::<ReaderCommand>();
        let proxy = self.link.proxy.clone();
        let (raw_tx, raw_rx) = mpsc::channel::<ReaderEvent>();
        if let Some(proxy) = proxy {
            std::thread::Builder::new()
                .name("winc-dap-events".into())
                .spawn(move || {
                    while let Ok(event) = raw_rx.recv() {
                        let ticket = match &event {
                            ReaderEvent::Connected(g, _)
                            | ReaderEvent::Response(g, _, _)
                            | ReaderEvent::Event(g, _)
                            | ReaderEvent::Closed(g) => *g,
                        };
                        if proxy
                            .send_event(UserEvent::Debug(ticket, Box::new(event)))
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .expect("spawn winc-dap-events");
        }
        self.debug.reader = if self.link.proxy.is_none() {
            Some(event_rx)
        } else {
            None
        };
        self.debug.writer = Some(command_tx);
        if self.link.proxy.is_none() {
            std::thread::Builder::new()
                .name("winc-dap-session".into())
                .spawn(move || run_debug_session(generation, launch, event_tx, command_rx))
                .expect("spawn winc-dap-session");
        } else {
            std::thread::Builder::new()
                .name("winc-dap-session".into())
                .spawn(move || run_debug_session(generation, launch, raw_tx, command_rx))
                .expect("spawn winc-dap-session");
        }
        self.request_redraw();
    }

    pub(in crate::app) fn stop_debug(&mut self) {
        if self.debug_active() {
            self.debug_request(
                Pending::Disconnect,
                "disconnect",
                json!({"terminateDebuggee": true}),
            );
        }
        self.stop_debug_session();
        self.debug.generation = self.debug.generation.next();
        self.debug.status = DebugStatus::Terminated;
        self.debug.stack.clear();
        self.debug.scopes.clear();
        self.debug.threads.clear();
        self.debug.active_thread = None;
        self.debug.launch_deadline = None;
        self.request_redraw();
    }

    fn stop_debug_session(&mut self) {
        if let Some(writer) = self.debug.writer.take() {
            let _ = writer.send(ReaderCommand {
                generation: self.debug.generation,
                id: 0,
                method: String::new(),
                params: Value::Null,
                disconnect: true,
            });
        }
        self.debug.reader = None;
        self.debug.pending = None;
    }

    pub(in crate::app) fn debug_continue(&mut self) {
        self.debug_control("continue");
    }
    pub(in crate::app) fn debug_next(&mut self) {
        self.debug_control("next");
    }
    pub(in crate::app) fn debug_step_in(&mut self) {
        self.debug_control("stepIn");
    }
    pub(in crate::app) fn debug_step_out(&mut self) {
        self.debug_control("stepOut");
    }
    fn debug_control(&mut self, command: &'static str) {
        let Some(thread) = self.debug.active_thread else {
            return;
        };
        self.debug_request(Pending::Control, command, json!({"threadId": thread}));
    }

    pub(in crate::app) fn load_debug_breakpoints(&mut self) {
        let Some(root) = self.editor.root().map(Path::to_path_buf) else {
            self.debug.breakpoints.clear();
            return;
        };
        let stored = self.state.debug_breakpoints(&root);
        self.debug.breakpoints.clear();
        for (path, entries) in stored {
            let document = self
                .editor
                .documents
                .iter()
                .find(|doc| doc.path.as_deref() == Some(path.as_path()))
                .map_or(DocumentId(0), |doc| doc.id);
            for (line, column, enabled) in entries {
                self.debug.breakpoints.push(Breakpoint {
                    document,
                    path: path.clone(),
                    line,
                    column,
                    enabled,
                    verified: false,
                    message: None,
                });
            }
        }
        self.request_redraw();
    }

    pub(in crate::app) fn toggle_debug_breakpoint_at_caret(&mut self) {
        let Some(doc) = self
            .editor
            .active()
            .filter(|doc| doc.path.is_some() && doc.shows_text())
        else {
            return;
        };
        let line = doc.text().line_of(doc.selection.head);
        let document = doc.id;
        self.click_debug_breakpoint(document, line);
    }

    pub(in crate::app) fn click_debug_breakpoint(&mut self, document: DocumentId, line: usize) {
        let path = self
            .editor
            .document(document)
            .and_then(|doc| doc.path.clone());
        let Some(path) = path else {
            return;
        };
        if let Some(index) = self
            .debug
            .breakpoints
            .iter()
            .position(|bp| bp.path == path && bp.line == line)
        {
            let removed = self.debug.breakpoints.remove(index);
            self.debug_sync_path(&removed.path);
        } else {
            self.debug.breakpoints.push(Breakpoint {
                document,
                path: path.clone(),
                line,
                column: 0,
                enabled: true,
                verified: false,
                message: None,
            });
            self.debug_sync_path(&path);
        }
        self.remember_debug_breakpoints();
        self.request_redraw();
    }

    fn remember_debug_breakpoints(&mut self) {
        let Some(root) = self.editor.root().map(Path::to_path_buf) else {
            return;
        };
        let mut by_path: std::collections::BTreeMap<PathBuf, Vec<(usize, usize, bool)>> =
            std::collections::BTreeMap::new();
        for bp in &self.debug.breakpoints {
            by_path
                .entry(bp.path.clone())
                .or_default()
                .push((bp.line, bp.column, bp.enabled));
        }
        let entries: Vec<_> = by_path.into_iter().collect();
        self.update_state(move |state| state.remember_debug_breakpoints(&root, entries));
    }

    fn debug_sync_path(&mut self, path: &Path) {
        if !self.debug_active() {
            return;
        }
        if self.debug.pending.is_some() {
            self.debug.pending_breakpoint_paths.push(path.to_path_buf());
            return;
        }
        let breakpoints = self
            .debug
            .breakpoints
            .iter()
            .filter(|bp| bp.path == path && bp.enabled)
            .map(|bp| SourceBreakpoint {
                line: bp.line.saturating_add(1) as u32,
                column: Some(bp.column.saturating_add(1) as u32),
            })
            .collect();
        let source = Source {
            name: path.file_name().and_then(|n| n.to_str()).map(str::to_owned),
            path: Some(path.to_string_lossy().into_owned()),
            source_reference: None,
        };
        let params = serde_json::to_value(SetBreakpointsRequest {
            source,
            breakpoints,
        })
        .unwrap_or(Value::Null);
        self.debug_request(
            Pending::Breakpoints(path.to_path_buf()),
            "setBreakpoints",
            params,
        );
    }

    fn sync_all_breakpoints(&mut self) {
        let paths: std::collections::BTreeSet<PathBuf> = self
            .debug
            .breakpoints
            .iter()
            .map(|bp| bp.path.clone())
            .collect();
        self.debug.pending_breakpoint_paths.extend(paths);
        self.flush_breakpoint_queue();
    }

    fn flush_breakpoint_queue(&mut self) {
        if self.debug.pending.is_some() {
            return;
        }
        while let Some(index) = self
            .debug
            .pending_breakpoint_paths
            .iter()
            .position(|path| self.debug.breakpoints.iter().any(|bp| &bp.path == path))
        {
            let path = self.debug.pending_breakpoint_paths.remove(index);
            self.debug_sync_path(&path);
            if self.debug.pending.is_some() {
                return;
            }
        }
        self.debug.pending_breakpoint_paths.clear();
    }

    fn debug_request(&mut self, pending: Pending, method: &str, params: Value) {
        if self.debug.pending.is_some() {
            return;
        }
        let Some(writer) = self.debug.writer.clone() else {
            return;
        };
        let generation = self.debug.generation;
        let id = self.debug.next_request;
        self.debug.next_request += 1;
        if writer
            .send(ReaderCommand {
                generation,
                id,
                method: method.to_owned(),
                params,
                disconnect: false,
            })
            .is_ok()
        {
            self.debug.pending = Some((id, generation, pending));
        }
    }

    /// Polls worker messages on the UI thread; stale generations are discarded.
    pub(in crate::app) fn poll_debug(&mut self) {
        let events = self
            .debug
            .reader
            .as_ref()
            .map(|rx| {
                let mut events = Vec::new();
                while let Ok(event) = rx.try_recv() {
                    events.push(event);
                }
                events
            })
            .unwrap_or_default();
        for event in events {
            match event {
                ReaderEvent::Connected(generation, Ok(())) => {
                    if generation != self.debug.generation {
                        continue;
                    }
                    let arguments =
                        serde_json::to_value(InitializeRequest::default()).unwrap_or(Value::Null);
                    self.debug_request(Pending::Initialize, "initialize", arguments);
                }
                ReaderEvent::Connected(generation, Err(error)) => {
                    self.debug_failed(generation, error)
                }
                ReaderEvent::Response(generation, id, result) => {
                    self.debug_response(generation, id, result)
                }
                ReaderEvent::Event(generation, event) => self.debug_event(generation, event),
                ReaderEvent::Closed(generation) => {
                    if generation == self.debug.generation && self.debug_active() {
                        self.debug_failed(
                            generation,
                            winc_i18n::t!("debug-adapter-disconnected").to_owned(),
                        );
                    }
                }
            }
        }
        if let Some(deadline) = self.debug.launch_deadline
            && Instant::now() >= deadline
            && self.debug.status == DebugStatus::Starting
        {
            self.debug_failed(
                self.debug.generation,
                winc_i18n::t!("debug-launch-timeout").to_owned(),
            );
        }
        self.flush_breakpoint_queue();
    }

    fn debug_response(&mut self, generation: Generation, id: u64, result: Result<Value, String>) {
        if generation != self.debug.generation {
            return;
        }
        let Some((expected, _, pending)) = self.debug.pending.take() else {
            return;
        };
        if expected != id {
            return;
        }
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                self.debug_failed(generation, error);
                return;
            }
        };
        match pending {
            Pending::Initialize => {
                let program = self
                    .debug
                    .program
                    .clone()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                self.debug_request(Pending::Launch, "launch", json!({"program": program, "cwd": self.debug.root.as_ref().map(|p| p.to_string_lossy().into_owned()), "stopOnEntry": false, "sourceLanguages": ["cangjie"]}));
            }
            Pending::Launch => {
                if self.debug.breakpoints.is_empty() {
                    self.finish_debug_configuration();
                } else {
                    self.sync_all_breakpoints();
                }
            }
            Pending::ConfigurationDone => {
                self.debug.configuration_done = true;
                self.debug.launch_deadline = None;
                self.debug.status = DebugStatus::Running;
                self.request_debug_threads();
            }
            Pending::Breakpoints(path) => {
                let response =
                    serde_json::from_value::<winc_dap::SetBreakpointsResponse>(value).ok();
                for (index, bp) in self
                    .debug
                    .breakpoints
                    .iter_mut()
                    .filter(|bp| bp.path == path && bp.enabled)
                    .enumerate()
                {
                    if let Some(answer) = response.as_ref().and_then(|r| r.breakpoints.get(index)) {
                        bp.verified = answer.verified;
                        bp.message.clone_from(&answer.message);
                    }
                }
                if self.debug.pending_breakpoint_paths.is_empty() && !self.debug.configuration_done
                {
                    self.finish_debug_configuration();
                } else {
                    self.flush_breakpoint_queue();
                }
            }
            Pending::Threads => {
                if let Ok(response) = serde_json::from_value::<winc_dap::ThreadsResponse>(value) {
                    self.debug.threads = response
                        .threads
                        .into_iter()
                        .map(|t| (t.id, t.name))
                        .collect();
                    if self.debug.active_thread.is_none() {
                        self.debug.active_thread = self.debug.threads.first().map(|t| t.0);
                    }
                }
            }
            Pending::Stack(thread) => {
                if let Ok(response) = serde_json::from_value::<winc_dap::StackTraceResponse>(value)
                {
                    self.debug.stack = response
                        .stack_frames
                        .into_iter()
                        .map(|f| StackFrame {
                            id: f.id,
                            name: f.name,
                            path: f.source.and_then(|s| s.path).map(PathBuf::from),
                            line: f.line.map(|n| (n as usize).saturating_sub(1)),
                            column: f.column.map(|n| (n as usize).saturating_sub(1)),
                        })
                        .collect();
                    self.debug.ui.selected_frame = 0;
                    let first = self.debug.stack.first().cloned();
                    if let Some(frame) = first {
                        self.request_debug_scopes(frame.id);
                        if let (Some(path), Some(line)) = (frame.path.as_deref(), frame.line) {
                            self.debug_open_location(path, line, frame.column.unwrap_or(0));
                        }
                    }
                }
                self.debug.active_thread = Some(thread);
                self.request_debug_threads();
            }
            Pending::Scopes(frame) => {
                if let Ok(response) = serde_json::from_value::<winc_dap::ScopesResponse>(value) {
                    self.debug.scopes = response
                        .scopes
                        .into_iter()
                        .map(|s| Scope {
                            reference: s.variables_reference,
                            name: s.name,
                            variables: Vec::new(),
                            loaded: false,
                        })
                        .collect();
                    self.debug.ui.selected_scope = 0;
                    let first = self.debug.scopes.first().map(|scope| scope.reference);
                    if let Some(reference) = first {
                        self.request_debug_variables(reference);
                    }
                }
                let _ = frame;
            }
            Pending::Variables(reference) => {
                if let Ok(response) = serde_json::from_value::<winc_dap::VariablesResponse>(value) {
                    let variables = response
                        .variables
                        .into_iter()
                        .map(variable_from_dap)
                        .collect();
                    if reference > 0 {
                        set_variable_children(&mut self.debug.scopes, reference, variables);
                    }
                }
            }
            Pending::Control => self.debug.status = DebugStatus::Running,
            Pending::Disconnect => {}
        }
        self.request_redraw();
    }

    fn finish_debug_configuration(&mut self) {
        self.debug_request(Pending::ConfigurationDone, "configurationDone", json!({}));
    }

    fn debug_event(&mut self, generation: Generation, event: DapEvent) {
        if generation != self.debug.generation {
            return;
        }
        match event {
            DapEvent::Stopped(body) => {
                let thread = body.get("threadId").and_then(Value::as_i64);
                let reason = body.get("reason").and_then(Value::as_str).unwrap_or("stop");
                self.debug.status = DebugStatus::Stopped;
                self.debug.active_thread = thread;
                self.debug_push_output("console", &winc_i18n::t!("debug-stopped", reason = reason));
                if let Some(thread) = thread {
                    self.request_debug_stack(thread);
                }
            }
            DapEvent::Output(body) => {
                let category = body
                    .get("category")
                    .and_then(Value::as_str)
                    .unwrap_or("console");
                if let Some(output) = body.get("output").and_then(Value::as_str) {
                    self.debug_push_output(category, output);
                }
                if let (Some(source), Some(line)) = (
                    body.get("source").and_then(Value::as_object),
                    body.get("line").and_then(Value::as_u64),
                ) && let Some(path) = source.get("path").and_then(Value::as_str)
                {
                    self.debug_open_location(
                        Path::new(path),
                        line.saturating_sub(1) as usize,
                        body.get("column")
                            .and_then(Value::as_u64)
                            .unwrap_or(1)
                            .saturating_sub(1) as usize,
                    );
                }
            }
            DapEvent::Terminated(_) => {
                self.debug.status = DebugStatus::Terminated;
                self.debug.stack.clear();
                self.debug.scopes.clear();
                self.debug.active_thread = None;
                self.debug.launch_deadline = None;
                self.stop_debug_session();
            }
            DapEvent::Disconnected(error) => self.debug_failed(generation, error),
            DapEvent::Protocol(error) => self.debug_push_output("adapter", &error),
            DapEvent::Other(_) => {}
        }
        self.request_redraw();
    }

    fn request_debug_threads(&mut self) {
        self.debug_request(Pending::Threads, "threads", json!({}));
    }
    fn request_debug_stack(&mut self, thread: i64) {
        self.debug_request(
            Pending::Stack(thread),
            "stackTrace",
            json!({"threadId": thread, "startFrame": 0, "levels": 100}),
        );
    }
    fn request_debug_scopes(&mut self, frame: i64) {
        self.debug_request(Pending::Scopes(frame), "scopes", json!({"frameId": frame}));
    }
    fn request_debug_variables(&mut self, reference: i64) {
        self.debug_request(
            Pending::Variables(reference),
            "variables",
            json!({"variablesReference": reference}),
        );
    }

    fn debug_push_output(&mut self, category: &str, text: &str) {
        if self.debug.output.chars().count() + text.chars().count() > MAX_OUTPUT_CHARS {
            self.debug.output_truncated = true;
            let remove = self
                .debug
                .output
                .chars()
                .count()
                .saturating_sub(MAX_OUTPUT_CHARS / 2);
            if let Some((index, _)) = self.debug.output.char_indices().nth(remove) {
                self.debug.output.drain(..index);
            }
        }
        if matches!(category, "stderr" | "important") {
            self.debug.output.push_str("⚠ ");
        }
        self.debug.output.push_str(text);
        if self.debug.ui.open {
            self.debug.ui.output_scroll = usize::MAX;
        }
    }

    fn debug_failed(&mut self, generation: Generation, error: String) {
        if generation != self.debug.generation {
            return;
        }
        self.debug.status = DebugStatus::Failed;
        self.debug.error = Some(error.clone());
        self.debug.launch_deadline = None;
        self.debug_push_output("stderr", &error);
        self.stop_debug_session();
        self.notify(NoticeKind::Error, error);
        self.request_redraw();
    }

    pub(in crate::app) fn debug_open_location(
        &mut self,
        path: &Path,
        line: usize,
        utf16_column: usize,
    ) {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.debug.root.clone().unwrap_or_default().join(path)
        };
        self.open_path_then(&path, move |app| {
            let Some(doc) = app.editor.active() else {
                return;
            };
            let line = line.min(doc.text().line_count().saturating_sub(1));
            let start = doc.text().line_start(line);
            let end = doc.text().line_end(line);
            let text = doc.text().substring(start, end);
            let column = winc_dap::utf16_to_char(&text, utf16_column).unwrap_or(0);
            app.jump_to(line, column, 0);
        });
    }

    pub(in crate::app) fn select_debug_frame(&mut self, index: usize) {
        let Some(frame) = self.debug.stack.get(index).cloned() else {
            return;
        };
        self.debug.ui.selected_frame = index;
        if let (Some(path), Some(line)) = (frame.path.as_deref(), frame.line) {
            self.debug_open_location(path, line, frame.column.unwrap_or(0));
        }
        self.request_debug_scopes(frame.id);
        self.request_redraw();
    }
    pub(in crate::app) fn select_debug_scope(&mut self, index: usize) {
        if self.debug.scopes.get(index).is_none() {
            return;
        }
        self.debug.ui.selected_scope = index;
        let reference = self.debug.scopes[index].reference;
        if !self.debug.scopes[index].loaded {
            self.request_debug_variables(reference);
        }
        self.request_redraw();
    }
    pub(in crate::app) fn toggle_debug_variable(&mut self, path: DebugPath) {
        let Some(scope_index) = path.get(0) else {
            return;
        };
        let Some(variable_index) = path.get(1) else {
            return;
        };
        let (reference, expanded, empty) = {
            let Some(scope) = self.debug.scopes.get_mut(scope_index) else {
                return;
            };
            let Some(variable) = scope.variables.get_mut(variable_index) else {
                return;
            };
            toggle_variable(variable, path, 2)
        };
        if expanded && empty {
            self.request_debug_variables(reference);
        }
        self.request_redraw();
    }
    pub(in crate::app) fn press_debug_control(&mut self, control: DebugControl) {
        match control {
            DebugControl::Start => self.start_debug(),
            DebugControl::Stop => self.stop_debug(),
            DebugControl::Continue => self.debug_continue(),
            DebugControl::Next => self.debug_next(),
            DebugControl::StepIn => self.debug_step_in(),
            DebugControl::StepOut => self.debug_step_out(),
            DebugControl::Close => self.close_debug_panel(),
        }
        self.request_redraw();
    }
}

fn run_debug_session(
    generation: Generation,
    launch: DebugLaunch,
    events: mpsc::Sender<ReaderEvent>,
    commands: mpsc::Receiver<ReaderCommand>,
) {
    if launch.program.as_os_str().is_empty() {
        let _ = events.send(ReaderEvent::Connected(
            generation,
            Err(winc_i18n::t!("debug-build-artifact-missing").to_owned()),
        ));
        return;
    }
    if !launch.config.args.is_empty() {
        let mut command = std::process::Command::new(&launch.config.command);
        command.args(&launch.config.args).current_dir(&launch.root);
        for (key, value) in debug_adapter_env(&launch.adapter) {
            command.env(key, value);
        }
        if let Some(root) = launch.adapter.ancestors().nth(4) {
            command.env("CANGJIE_HOME", root);
        }
        let output = command.output();
        match output {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let detail = if stderr.trim().is_empty() {
                    stdout
                } else {
                    stderr
                };
                let _ = events.send(ReaderEvent::Connected(
                    generation,
                    Err(detail
                        .chars()
                        .rev()
                        .take(4000)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect()),
                ));
                return;
            }
            Err(error) => {
                let _ = events.send(ReaderEvent::Connected(generation, Err(error.to_string())));
                return;
            }
        }
    }
    if !launch.program.is_file() {
        let _ = events.send(ReaderEvent::Connected(
            generation,
            Err(winc_i18n::t!("debug-build-artifact-missing").to_owned()),
        ));
        return;
    }
    let mut tcp = TcpConfig::new(launch.adapter.clone());
    tcp.cwd = Some(launch.root.clone());
    tcp.env = debug_adapter_env(&launch.adapter);
    let connected = winc_dap::Client::connect(tcp);
    let mut client = match connected {
        Ok(connected) => connected.client,
        Err(error) => {
            let _ = events.send(ReaderEvent::Connected(generation, Err(error.to_string())));
            return;
        }
    };
    if events
        .send(ReaderEvent::Connected(generation, Ok(())))
        .is_err()
    {
        return;
    }
    while let Ok(command) = commands.recv() {
        if command.disconnect || command.generation != generation {
            break;
        }
        if command.method == "disconnect" {
            let _ = client
                .request("disconnect", command.params)
                .map_err(|e| std::io::Error::other(e.to_string()))
                .and_then(|r| {
                    r.recv_timeout(REQUEST_TIMEOUT)
                        .map_err(|e| std::io::Error::other(e.to_string()))
                });
            break;
        }
        let answer = client
            .request(&command.method, command.params)
            .map_err(|e| e.to_string())
            .and_then(|r| r.recv_timeout(REQUEST_TIMEOUT).map_err(|e| e.to_string()));
        if events
            .send(ReaderEvent::Response(generation, command.id, answer))
            .is_err()
        {
            break;
        }
        while let Some(event) = client.try_next_event() {
            if events.send(ReaderEvent::Event(generation, event)).is_err() {
                return;
            }
        }
    }
    let _ = events.send(ReaderEvent::Closed(generation));
}

/// LLDB and its DAP adapter load libraries from the SDK, not from PATH.
fn debug_adapter_env(adapter: &Path) -> Vec<(String, String)> {
    let mut dirs = Vec::new();
    if let Some(root) = adapter.ancestors().nth(4) {
        dirs.extend([
            root.join("third_party/llvm/lib"),
            root.join("tools/lib"),
            root.join("lib"),
        ]);
        if let Ok(entries) = std::fs::read_dir(root.join("runtime/lib")) {
            dirs.extend(
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.is_dir()),
            );
        }
    }
    if let Some(path) = std::env::var_os("LD_LIBRARY_PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    let mut env = Vec::new();
    let python = python311_home();
    if let Some(python) = &python {
        dirs.insert(0, python.join("lib"));
    }
    if let Some(value) = std::env::join_paths(dirs)
        .ok()
        .and_then(|value| value.into_string().ok())
    {
        env.push(("LD_LIBRARY_PATH".to_owned(), value));
    }
    if let Some(python) = python {
        env.push((
            "PYTHONHOME".to_owned(),
            python.to_string_lossy().into_owned(),
        ));
    }
    env
}

/// LLDB 15 in the Cangjie SDK links against Python 3.11. Prefer `WINC_PYTHON311_HOME`,
/// then the locally installed runtime; the system runtime is used as-is when present.
fn python311_home() -> Option<PathBuf> {
    let has_lib = |home: &Path| home.join("lib/libpython3.11.so.1.0").is_file();
    if let Some(home) = std::env::var_os("WINC_PYTHON311_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        && has_lib(&home)
    {
        return Some(home);
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let find = |base: PathBuf| {
        std::fs::read_dir(base)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("python-3.11"))
                    && has_lib(path)
            })
    };
    find(PathBuf::from(&home).join(".local/share/cangjie"))
        .or_else(|| find(PathBuf::from(home).join(".local/share/winc")))
}

fn variable_from_dap(variable: winc_dap::Variable) -> Variable {
    Variable {
        name: variable.name,
        value: variable.value,
        kind: variable.r#type,
        reference: variable.variables_reference,
        expanded: false,
        children: Vec::new(),
    }
}

fn toggle_variable(variable: &mut Variable, path: DebugPath, depth: usize) -> (i64, bool, bool) {
    let Some(index) = path.get(depth) else {
        if variable.reference != 0 {
            variable.expanded = !variable.expanded;
        }
        return (
            variable.reference,
            variable.expanded,
            variable.children.is_empty(),
        );
    };
    let Some(child) = variable.children.get_mut(index) else {
        return (0, false, false);
    };
    toggle_variable(child, path, depth + 1)
}
fn set_variable_children(scopes: &mut [Scope], reference: i64, variables: Vec<Variable>) -> bool {
    for scope in scopes {
        if scope.reference == reference {
            scope.variables = variables;
            scope.loaded = true;
            return true;
        }
        if set_variable_node(&mut scope.variables, reference, variables.clone()) {
            return true;
        }
    }
    false
}
fn set_variable_node(variables: &mut [Variable], reference: i64, children: Vec<Variable>) -> bool {
    for variable in variables {
        if variable.reference == reference {
            variable.children = children;
            variable.expanded = true;
            return true;
        }
        if set_variable_node(&mut variable.children, reference, children.clone()) {
            return true;
        }
    }
    false
}
fn resolve_program(root: &Path, configured: &str) -> PathBuf {
    let path = PathBuf::from(configured);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

fn built_program(root: &Path) -> PathBuf {
    let name = std::fs::read_to_string(root.join("cjpm.toml"))
        .ok()
        .and_then(|text| {
            let package = text
                .split("[package]")
                .nth(1)
                .map(|part| part.split('[').next().unwrap_or_default())?;
            package
                .lines()
                .find_map(|line| line.trim().strip_prefix("name")?.split_once('='))
                .map(|(_, value)| value.trim().trim_matches(['"', '\'']).to_owned())
        })
        .unwrap_or_else(|| {
            root.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
    for relative in [
        "target/debug/bin/main".to_owned(),
        format!("target/debug/bin/{name}"),
        "target/release/bin/main".to_owned(),
        format!("target/release/bin/{name}"),
        format!("target/debug/{name}"),
        format!("target/release/{name}"),
    ] {
        let path = root.join(relative);
        if path.is_file() {
            return path;
        }
    }
    root.join("target/debug/bin/main")
}

fn debug_adapter_path(
    root: &Path,
    configured: Option<&str>,
    configured_sdk: Option<&str>,
) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("WINC_DEBUG_ADAPTER").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let configured = configured
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let root = crate::app::cangjie::sdk_root(configured_sdk)?;
            Some(root.join("third_party/llvm/bin/lldb-vscode"))
        })?;
    let path = if configured.is_absolute() {
        configured
    } else {
        root.join(configured)
    };
    path.is_file().then_some(path)
}

impl App {
    /// Applies one event delivered through `UserEvent::Debug`.
    pub(in crate::app) fn handle_debug_event(
        &mut self,
        generation: Generation,
        event: ReaderEvent,
    ) {
        if generation != self.debug.generation {
            return;
        }
        match event {
            ReaderEvent::Connected(_, Ok(())) => {
                let arguments =
                    serde_json::to_value(InitializeRequest::default()).unwrap_or(Value::Null);
                self.debug_request(Pending::Initialize, "initialize", arguments);
            }
            ReaderEvent::Connected(_, Err(error)) => self.debug_failed(generation, error),
            ReaderEvent::Response(_, id, result) => self.debug_response(generation, id, result),
            ReaderEvent::Event(_, event) => self.debug_event(generation, event),
            ReaderEvent::Closed(_) => {
                if self.debug_active() {
                    self.debug_failed(
                        generation,
                        winc_i18n::t!("debug-adapter-disconnected").to_owned(),
                    );
                }
            }
        }
        self.request_redraw();
    }
}
