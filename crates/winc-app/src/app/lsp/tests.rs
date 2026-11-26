//! app 语言服务集成测试：假 stdio server 验证初始化、文档同步、诊断、异常与关闭。

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use winc_config::Config;
use winc_core::Lines;
use winc_editor::Editor;
use winc_lsp::{Event, Position, ServerConfig};

use super::*;
use crate::app::{App, StatusItem};
use crate::bench::Bench;

const SERVER: &str = r#"
import json, os, sys, time
log = os.environ.get("LSP_TEST_LOG")
mode = os.environ.get("LSP_TEST_MODE", "normal")

def send(value):
    data = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()
    sys.stdout.buffer.write(b"Content-Length: " + str(len(data)).encode() + b"\r\n\r\n" + data)
    sys.stdout.buffer.flush()

def record(value):
    if log:
        with open(log, "a", encoding="utf-8") as out:
            out.write(json.dumps(value, ensure_ascii=False) + "\n")

def read():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        key, value = line.decode().split(":", 1)
        headers[key.lower()] = value.strip()
    length = int(headers["content-length"])
    return json.loads(sys.stdin.buffer.read(length))

record({"args": sys.argv[1:]})
while True:
    message = read()
    if message is None:
        break
    record(message)
    method = message.get("method")
    if method == "initialize":
        if mode == "bad-initialize":
            send({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32603, "message": "initialize failed"}})
            continue
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"capabilities": {"textDocumentSync": 2}, "serverInfo": {"name": "fake", "version": "1"}}})
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": message["id"], "result": None})
    elif method == "exit":
        break
    elif method == "textDocument/didOpen":
        if mode == "exit-on-open":
            send({"jsonrpc": "2.0", "method": "exit", "params": None})
            break
        text = message["params"]["textDocument"]["text"]
        diagnostics = []
        if mode == "diagnostics" and "main" in text:
            diagnostics = [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}, "severity": 1, "source": "fake", "message": "main 有问题"}]
        send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": message["params"]["textDocument"]["uri"], "version": message["params"]["textDocument"]["version"], "diagnostics": diagnostics}})
    elif method == "textDocument/didChange":
        send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": message["params"]["textDocument"]["uri"], "version": message["params"]["textDocument"]["version"], "diagnostics": []}})
"#;

#[derive(Debug)]
struct Log(PathBuf);

impl Log {
    fn read(path: &Path) -> Self {
        Self(path.to_path_buf())
    }

    fn values(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.0)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    fn methods(&self) -> Vec<String> {
        self.values()
            .into_iter()
            .filter_map(|value| {
                value
                    .get("method")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect()
    }

    fn wait_for(&self, method: &str) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let values = self.values();
            if values
                .iter()
                .any(|value| value.get("method").and_then(Value::as_str) == Some(method))
            {
                return values;
            }
            assert!(
                Instant::now() < deadline,
                "server did not receive {method}: {values:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

struct Fixture {
    dir: PathBuf,
    log: Log,
    app: App,
}

impl Fixture {
    fn new(name: &str, mode: &str, text: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "winc-app-lsp-{name}-{}-{serial}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = dir.join("fake-lsp.py");
        std::fs::write(&server, SERVER).unwrap();
        let python = if cfg!(windows) { "python" } else { "python3" };
        let launcher = dir.join(if cfg!(windows) {
            "LSPServer.bat"
        } else {
            "LSPServer"
        });
        if cfg!(windows) {
            std::fs::write(
                &launcher,
                format!("@echo off\r\n\"{python}\" \"{}\" %*\r\n", server.display()),
            )
            .unwrap();
        } else {
            winc_backend::testing::write_script(
                &launcher,
                &format!("#!/bin/sh\nexec {python} '{}' \"$@\"\n", server.display()),
            );
        }
        let log_path = dir.join("messages.jsonl");
        let config = ServerConfig::new(&launcher)
            .env("LSP_TEST_LOG", log_path.to_string_lossy())
            .env("LSP_TEST_MODE", mode)
            .cangjie_defaults(false);
        let mut editor = Editor::default();
        let path = dir.join("main.cj");
        std::fs::write(&path, text).unwrap();
        let index = editor.open(&path);
        editor.active = Some(index);
        let mut app = App::new(editor, None, Bench::off(), Config::default());
        app.lsp.test_server = Some(config);
        app.ensure_lsp_started();
        Self {
            dir,
            log: Log::read(&log_path),
            app,
        }
    }

    fn reload_log(&mut self) {
        self.log = Log::read(&self.dir.join("messages.jsonl"));
    }

    fn dispatch_until(&mut self, predicate: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let events = self
                .app
                .lsp
                .client
                .as_ref()
                .and_then(|client| client.try_recv_event().ok());
            if let Some(event) = events {
                self.app
                    .handle_lsp_events(self.app.lsp.generation(), vec![event]);
                if predicate(&self.app) {
                    return;
                }
            }
            assert!(
                Instant::now() < deadline,
                "condition not reached; status={:?}",
                self.app.lsp.status
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.app.close_lsp();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn initializes_and_synchronizes_document_lifecycle() {
    let mut fixture = Fixture::new("lifecycle", "diagnostics", "main() {}\n");
    assert!(
        !fixture.app.lsp.open.is_empty(),
        "open={:?} docs={:?} status={:?}",
        fixture.app.lsp.open,
        fixture
            .app
            .editor
            .documents
            .iter()
            .map(|doc| {
                (
                    doc.id,
                    doc.highlighter.language().id,
                    doc.path.clone(),
                    doc.is_pending(),
                    doc.is_large(),
                    doc.shows_text(),
                    doc.text().end_pos(),
                )
            })
            .collect::<Vec<_>>(),
        fixture.app.lsp.status,
    );
    fixture.reload_log();
    let values = fixture.log.wait_for("textDocument/didOpen");
    let initialize = values
        .iter()
        .find(|value| value.get("method").and_then(Value::as_str) == Some("initialize"))
        .unwrap();
    assert_eq!(
        initialize["params"]["rootUri"],
        json!(file_uri(&fixture.dir.join("main.cj")))
    );
    assert_eq!(
        initialize["params"]["initializationOptions"]["singleFilePath"],
        json!(file_uri(&fixture.dir.join("main.cj")))
    );
    assert!(
        values
            .iter()
            .any(|value| value.get("method").and_then(Value::as_str) == Some("initialized"))
    );
    let open = values
        .iter()
        .find(|value| value.get("method").and_then(Value::as_str) == Some("textDocument/didOpen"))
        .unwrap();
    assert_eq!(open["params"]["textDocument"]["languageId"], "Cangjie");
    assert_eq!(open["params"]["textDocument"]["text"], "main() {}\n");
    assert_eq!(fixture.app.lsp.status, Status::Ready);

    fixture.dispatch_until(|app| app.lsp.problem_count(app.editor.active().map(|doc| doc.id)) == 1);
    let id = fixture.app.editor.active().unwrap().id;
    let diagnostic = &fixture.app.lsp.diagnostics[&id][0];
    assert_eq!(diagnostic.message, "main 有问题");
    assert_eq!(diagnostic.range.start, 0);

    let before = fixture.app.editor.active().unwrap().revision;
    fixture
        .app
        .run(winc_editor::Command::Insert("// 改\n".into()));
    fixture.app.sync_lsp_documents();
    assert!(fixture.app.editor.active().unwrap().revision > before);
    fixture.reload_log();
    fixture.log.wait_for("textDocument/didChange");
    fixture.dispatch_until(|app| app.lsp.problem_count(app.editor.active().map(|doc| doc.id)) == 0);

    fixture.app.close_documents(&[id]);
    fixture.app.sync_lsp_documents();
    fixture.reload_log();
    let values = fixture.log.wait_for("textDocument/didClose");
    assert!(
        values
            .iter()
            .any(|value| value.get("method").and_then(Value::as_str)
                == Some("textDocument/didClose"))
    );
    assert!(fixture.app.lsp.open.is_empty());
}

#[test]
fn server_exit_marks_failed_and_click_restarts() {
    let mut fixture = Fixture::new("exit", "exit-on-open", "main() {}\n");
    fixture.dispatch_until(|app| app.lsp.status == Status::Failed);
    fixture.app.lsp.test_server = Some(
        ServerConfig::new(fixture.dir.join("LSPServer"))
            .env(
                "LSP_TEST_LOG",
                fixture.dir.join("messages.jsonl").to_string_lossy(),
            )
            .env("LSP_TEST_MODE", "diagnostics")
            .cangjie_defaults(false),
    );
    fixture.app.open_status_menu(StatusItem::LanguageService);
    assert_eq!(fixture.app.lsp.status, Status::Ready);
}

#[test]
fn initialize_failure_is_unavailable_for_editing() {
    let mut fixture = Fixture::new("bad-initialize", "bad-initialize", "main() {}\n");
    assert_eq!(fixture.app.lsp.status, Status::Failed);
    fixture
        .app
        .run(winc_editor::Command::Insert("// 仍可编辑\n".into()));
    let doc = fixture.app.editor.active().unwrap();
    assert!(
        doc.text()
            .substring(0, doc.text().end_pos())
            .contains("仍可编辑")
    );
}

#[test]
fn shutdown_exit_and_close_leave_no_open_documents() {
    let mut fixture = Fixture::new("shutdown", "normal", "main() {}\n");
    fixture.app.close_lsp();
    fixture.reload_log();
    let values = fixture.log.wait_for("exit");
    let methods = fixture.log.methods();
    assert!(methods.iter().any(|method| method == "shutdown"));
    assert!(methods.iter().any(|method| method == "exit"));
    assert!(fixture.app.lsp.open.is_empty());
    assert!(fixture.app.lsp.diagnostics.is_empty());
    assert!(fixture.app.lsp.client.is_none());
    assert!(!values.is_empty());
}

#[test]
fn sdk_discovery_checks_path_cangjie_home_and_cjv() {
    let root = std::env::temp_dir().join(format!("winc-lsp-discovery-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("tools/bin")).unwrap();
    let server = root.join("tools/bin").join(if cfg!(windows) {
        "LSPServer.exe"
    } else {
        "LSPServer"
    });
    std::fs::write(&server, "").unwrap();
    let found = find_lsp_server_with(&[
        ("CANGJIE_HOME", root.as_os_str()),
        ("HOME", root.join("missing-home").as_os_str()),
        ("PATH", root.join("missing-path").as_os_str()),
    ]);
    assert_eq!(found, Some(server));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn file_uris_encode_spaces_and_windows_drive_paths() {
    #[cfg(windows)]
    assert_eq!(
        file_uri(Path::new(r"C:\a b\main.cj")),
        "file:///C:/a%20b/main.cj"
    );
    #[cfg(not(windows))]
    assert_eq!(
        file_uri(Path::new("/a b/主.cj")),
        "file:///a%20b/%E4%B8%BB.cj"
    );
}

#[test]
fn utf16_diagnostics_map_supplementary_characters_to_char_offsets() {
    let dir = std::env::temp_dir().join(format!("winc-app-lsp-unicode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("unicode.cj");
    std::fs::write(&path, "😀main\n").unwrap();
    let mut editor = Editor::default();
    let index = editor.open(&path);
    editor.active = Some(index);
    let mut app = App::new(editor, None, Bench::off(), Config::default());
    let id = app.editor.active().unwrap().id;
    app.lsp.status = Status::Ready;
    app.lsp.open.insert(
        id,
        OpenDocument {
            path: path.clone(),
            uri: file_uri(&path),
            version: 1,
            revision: app.editor.active().unwrap().revision,
        },
    );
    let source = app
        .editor
        .active()
        .unwrap()
        .text()
        .substring(0, app.editor.active().unwrap().text().end_pos());
    assert_eq!(source, "😀main\n");
    assert_eq!(Position::new(0, 2).to_char_offset(&source), Some(1));
    assert_eq!(Position::new(0, 6).to_char_offset(&source), Some(5));
    let diagnostics = vec![winc_lsp::Diagnostic {
        range: winc_lsp::Range::new(Position::new(0, 2), Position::new(0, 6)),
        severity: Some(1),
        code: None,
        code_description: None,
        source: None,
        message: "unicode".into(),
        tags: Vec::new(),
        related_information: None,
        data: None,
    }];
    app.handle_lsp_events(
        app.lsp.generation(),
        vec![Event::Diagnostics {
            uri: file_uri(&path),
            version: Some(1),
            diagnostics,
        }],
    );
    assert_eq!(app.lsp.diagnostics[&id][0].range.start, 1);
    assert_eq!(app.lsp.diagnostics[&id][0].version, 1);
    assert_eq!(app.lsp.diagnostics[&id][0].range.end, 5);
    app.close_lsp();
    let _ = std::fs::remove_dir_all(&dir);
}

fn find_lsp_server_with(vars: &[(&str, &std::ffi::OsStr)]) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "LSPServer.exe"
    } else {
        "LSPServer"
    };
    for (key, value) in vars {
        match *key {
            "PATH" => {
                if let Some(path) =
                    std::env::split_paths(value).find_map(|dir| executable(&dir.join(exe)))
                {
                    return Some(path);
                }
            }
            "CANGJIE_HOME" => {
                let root = PathBuf::from(value);
                for path in [root.join("tools/bin").join(exe), root.join("bin").join(exe)] {
                    if let Some(path) = executable(&path) {
                        return Some(path);
                    }
                }
            }
            "HOME" | "USERPROFILE" => {
                let cjv = PathBuf::from(value).join(".cjv");
                if let Some(path) = executable(&cjv.join("bin").join(exe)) {
                    return Some(path);
                }
                if let Ok(entries) = std::fs::read_dir(cjv.join("toolchains")) {
                    for entry in entries.flatten() {
                        for path in [
                            entry.path().join("tools/bin").join(exe),
                            entry.path().join("bin").join(exe),
                        ] {
                            if let Some(path) = executable(&path) {
                                return Some(path);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}
