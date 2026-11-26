//! Loopback TCP transport for `lldb-vscode --port`.

use std::collections::HashMap;
use std::io::Read;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use serde_json::{Value, json};

use crate::framing::{DecodeError, FramedReader, FramedWriter};
use crate::jsonrpc::{Id, Message, Notification, Request, RequestError, Response};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

type PendingMap = Arc<Mutex<HashMap<Id, mpsc::Sender<Result<Value, RequestError>>>>>;

/// Process and connection settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpConfig {
    /// `lldb-vscode` executable.
    pub adapter: std::path::PathBuf,
    /// Arguments passed before `--port`.
    pub adapter_args: Vec<String>,
    /// Loopback address where the adapter listens; port zero asks the OS for an unused port.
    pub address: SocketAddr,
    /// Environment additions for the adapter.
    pub env: Vec<(String, String)>,
    /// Optional adapter working directory.
    pub cwd: Option<std::path::PathBuf>,
}

impl TcpConfig {
    /// Creates a config that listens on an OS-selected IPv4 loopback port.
    pub fn new(adapter: impl Into<std::path::PathBuf>) -> Self {
        Self {
            adapter: adapter.into(),
            adapter_args: Vec::new(),
            address: SocketAddr::from(([127, 0, 0, 1], 0)),
            env: Vec::new(),
            cwd: None,
        }
    }
}

/// Events delivered by the adapter reader thread.
#[derive(Debug, Clone)]
pub enum Event {
    /// Adapter stopped.
    Stopped(Value),
    /// Adapter wrote output.
    Output(Value),
    /// Debuggee terminated.
    Terminated(Value),
    /// Adapter event that the client does not interpret (for example `initialized`,
    /// `process`, `thread`, `module`). Kept as JSON for callers that need it.
    Other(Value),
    /// Adapter process or TCP stream disconnected.
    Disconnected(String),
    /// A malformed adapter message was ignored.
    Protocol(String),
}

/// A pending response receiver. Dropping it cancels local waiting only.
#[derive(Debug)]
pub struct PendingRequest {
    receiver: mpsc::Receiver<Result<Value, RequestError>>,
}

impl PendingRequest {
    /// Waits for the response.
    ///
    /// # Errors
    /// Returns adapter errors, disconnect, or timeout.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Value, RequestError> {
        self.receiver
            .recv_timeout(timeout)
            .map_err(|_| RequestError::Closed)?
    }

    /// Stops waiting locally.
    pub fn cancel(&self) {}
}

/// DAP adapter process and bidirectional channel.
pub struct Client {
    process: Child,
    writer: FramedWriter<TcpStream>,
    next_id: i64,
    pending: PendingMap,
    events: mpsc::Receiver<Event>,

    address: SocketAddr,
    initialized: bool,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("pid", &self.process.id())
            .field("address", &self.address)
            .field("initialized", &self.initialized)
            .finish_non_exhaustive()
    }
}

/// Result of launching and connecting an adapter.
pub struct Connect {
    /// Connected client.
    pub client: Client,
    /// Bound loopback endpoint, useful for diagnostics.
    pub address: SocketAddr,
}

impl Client {
    /// Spawns an adapter listening on loopback, connects, and returns after the socket handshake.
    ///
    /// The adapter's `--port N` arguments are appended to `adapter_args` and are not shell-interpreted.
    ///
    /// # Errors
    /// Returns process, connect, or timeout errors.
    pub fn connect(config: TcpConfig) -> Result<Connect, std::io::Error> {
        let port = if config.address.port() == 0 {
            TcpListener::bind(config.address)?.local_addr()?.port()
        } else {
            config.address.port()
        };
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let mut command = Command::new(&config.adapter);
        command
            .args(&config.adapter_args)
            .arg("--port")
            .arg(port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in &config.env {
            command.env(key, value);
        }
        let mut process = command.spawn()?;
        let stream = connect_to_adapter(address, &mut process)?;
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
        stream.set_nodelay(true)?;
        let read_stream = stream.try_clone()?;
        let writer = FramedWriter::new(stream);
        let (event_sender, events) = mpsc::channel();
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let stderr = process.stderr.take().map(|stderr| {
            let sender = event_sender.clone();
            std::thread::spawn(move || {
                let mut text = String::new();
                let mut reader = std::io::BufReader::new(stderr);
                let _ = reader.read_to_string(&mut text);
                if !text.is_empty() {
                    let _ = sender.send(Event::Output(json!({
                        "category": "stderr",
                        "output": text,
                    })));
                }
            })
        });
        let client = Client {
            process,
            writer,
            next_id: 1,
            pending: pending.clone(),
            events,

            address,
            initialized: false,
        };
        let reader_pending = Arc::clone(&pending);
        std::thread::Builder::new()
            .name("winc-dap-reader".into())
            .spawn(move || run_reader(read_stream, event_sender, reader_pending))
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        if let Some(stderr) = stderr {
            drop(stderr);
        }
        Ok(Connect { client, address })
    }

    /// Bound loopback endpoint.
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    /// Allocates and sends a request.
    ///
    /// # Errors
    /// Returns transport errors.
    pub fn request(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<PendingRequest, std::io::Error> {
        let id = Id::Number(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        let (sender, receiver) = mpsc::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id.clone(), sender);
        }
        let request = Request::new(id.clone(), method, Some(params));
        let value = serde_json::to_value(request)?;
        if let Err(error) = self.writer.write(&value) {
            if let Ok(mut pending) = self.pending.lock() {
                pending.remove(&id);
            }
            return Err(std::io::Error::other(error.to_string()));
        }
        Ok(PendingRequest { receiver })
    }

    /// Sends a notification without waiting for a response.
    ///
    /// # Errors
    /// Returns transport errors.
    pub fn notify(&mut self, event: &str, body: Value) -> Result<(), std::io::Error> {
        let seq = Id::Number(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        let value = serde_json::to_value(Notification::new(seq, event, Some(body)))?;
        self.writer
            .write(&value)
            .map_err(|error| std::io::Error::other(error.to_string()))
    }

    /// Sends initialize and waits for its response.
    ///
    /// # Errors
    /// Returns adapter or transport errors.
    pub fn initialize(&mut self, arguments: Value) -> Result<Value, RequestError> {
        let request = self.request("initialize", arguments)?;
        let result = request.recv_timeout(HANDSHAKE_TIMEOUT)?;
        self.initialized = true;
        Ok(result)
    }

    /// Marks the handshake complete.
    ///
    /// DAP has no client-to-adapter `initialized` notification: the adapter sends the
    /// `initialized` event instead. This only records the state.
    ///
    /// # Errors
    ///
    /// Currently never returns an error; kept for symmetry with [`initialize`](Self::initialize).
    pub fn initialized(&mut self) -> Result<(), std::io::Error> {
        self.initialized = true;
        Ok(())
    }

    /// Returns whether initialize has completed.
    pub const fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Takes the next adapter event, waiting up to `timeout`.
    pub fn next_event(&self, timeout: Duration) -> Option<Event> {
        self.events.recv_timeout(timeout).ok()
    }

    /// Takes an immediately available adapter event.
    pub fn try_next_event(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}

fn connect_to_adapter(
    address: SocketAddr,
    process: &mut Child,
) -> Result<TcpStream, std::io::Error> {
    let deadline = std::time::Instant::now() + HANDSHAKE_TIMEOUT;
    loop {
        match TcpStream::connect_timeout(&address, Duration::from_millis(200)) {
            Ok(stream) => return Ok(stream),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                ) =>
            {
                if let Some(status) = process.try_wait()? {
                    let stderr = process.stderr.take().map(|mut stderr| {
                        let mut text = String::new();
                        let _ = stderr.read_to_string(&mut text);
                        text
                    });
                    return Err(std::io::Error::other(format!(
                        "lldb-vscode exited with {status}{}",
                        stderr
                            .filter(|text| !text.trim().is_empty())
                            .map(|text| format!(": {text}"))
                            .unwrap_or_default()
                    )));
                }
                if std::time::Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "lldb-vscode did not listen on the loopback port",
                    ));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(error),
        }
    }
}

fn run_reader(stream: TcpStream, events: mpsc::Sender<Event>, pending: PendingMap) {
    let mut reader = FramedReader::new(stream);
    loop {
        let value = match reader.read() {
            Ok(value) => value,
            Err(DecodeError::Eof) => {
                let _ = events.send(Event::Disconnected(
                    "debug adapter closed the connection".into(),
                ));
                break;
            }
            Err(error) => {
                let _ = events.send(Event::Protocol(error.to_string()));
                break;
            }
        };
        match Message::from_value(value) {
            Ok(Message::Response(Response { id, result, error })) => {
                let answer = match error {
                    Some(error) => Err(RequestError::Protocol(error)),
                    None => Ok(result.unwrap_or(Value::Null)),
                };
                if let Ok(mut pending) = pending.lock()
                    && let Some(sender) = pending.remove(&id)
                {
                    let _ = sender.send(answer);
                }
            }
            Ok(Message::Event(event)) => {
                let body = event.body.unwrap_or(Value::Null);
                let event = match event.event.as_str() {
                    "stopped" => Event::Stopped(body),
                    "output" => Event::Output(body),
                    "terminated" => Event::Terminated(body),
                    other => Event::Other(json!({
                        "event": other,
                        "body": body,
                    })),
                };
                if events.send(event).is_err() {
                    break;
                }
            }
            Err(error) => {
                if events.send(Event::Protocol(error)).is_err() {
                    break;
                }
            }
        }
    }
    if let Ok(mut pending) = pending.lock() {
        for (_, sender) in pending.drain() {
            let _ = sender.send(Err(RequestError::Closed));
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const ADAPTER: &str = r#"
import json, socket, sys
port = int(sys.argv[-1])
srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", port))
srv.listen(1)
s, _ = srv.accept()
f = s.makefile("rwb")
def read():
    headers = {}
    while True:
        line = f.readline()
        if line in (b"\r\n", b"\n", b""):
            break
        key, value = line.decode().split(":", 1)
        headers[key.lower()] = value.strip()
    return json.loads(f.read(int(headers["content-length"])))
def send(value):
    body = json.dumps(value, separators=(",", ":")).encode()
    f.write(b"Content-Length: " + str(len(body)).encode() + b"\r\n\r\n" + body)
    f.flush()
while True:
    message = read()
    method = message.get("command")
    if method == "initialize":
        send({"seq": 0, "type": "response", "request_seq": message["seq"], "success": True, "command": method, "body": {"supportsConfigurationDoneRequest": True}})
    elif method == "setBreakpoints":
        send({"seq": 0, "type": "response", "request_seq": message["seq"], "success": True, "command": method, "body": {"breakpoints": [{"verified": True, "line": 3}]}})
    elif method == "stackTrace":
        send({"seq": 0, "type": "response", "request_seq": message["seq"], "success": True, "command": method, "body": {"stackFrames": [{"id": 7, "name": "main", "source": {"path": "/tmp/main.cj"}, "line": 3, "column": 1}]}})
    elif method == "scopes":
        send({"seq": 0, "type": "response", "request_seq": message["seq"], "success": True, "command": method, "body": {"scopes": [{"name": "Locals", "variablesReference": 11, "expensive": False}]}})
    elif method == "variables":
        send({"seq": 0, "type": "response", "request_seq": message["seq"], "success": True, "command": method, "body": {"variables": [{"name": "x", "value": "42", "variablesReference": 0}]}})
    elif method == "configurationDone":
        send({"seq": 0, "type": "response", "request_seq": message["seq"], "success": True, "command": method})
        send({"seq": 0, "type": "event", "event": "stopped", "body": {"reason": "breakpoint", "threadId": 1}})
    elif method == "continue":
        send({"seq": 0, "type": "response", "request_seq": message["seq"], "success": True, "command": method, "body": {"allThreadsContinued": True}})
    elif method == "disconnect":
        send({"seq": 0, "type": "response", "request_seq": message["seq"], "success": True, "command": method})
        break
"#;

    #[test]
    fn speaks_dap_over_loopback_and_handles_breakpoints_and_events() {
        let dir = std::env::temp_dir().join(format!("winc-dap-fake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("adapter.py");
        std::fs::write(&script, ADAPTER).unwrap();
        let adapter = dir.join("adapter");
        std::fs::write(
            &adapter,
            format!("#!/bin/sh\nexec python3 '{}' \"$@\"\n", script.display()),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let mut config = TcpConfig::new(&adapter);
        config.address = SocketAddr::from(([127, 0, 0, 1], 0));
        let connected = Client::connect(config).unwrap();
        assert_eq!(
            connected.address.ip(),
            std::net::IpAddr::from([127, 0, 0, 1])
        );
        let mut client = connected.client;

        let initialize = client.initialize(json!({})).unwrap();
        assert_eq!(initialize["supportsConfigurationDoneRequest"], true);
        client.initialized().unwrap();

        let breakpoints = client
            .request(
                "setBreakpoints",
                json!({"source": {"path": "/tmp/main.cj"}, "breakpoints": [{"line": 3}]}),
            )
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(breakpoints["breakpoints"][0]["verified"], true);

        client
            .request("configurationDone", json!({}))
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let event = client.next_event(Duration::from_secs(5)).unwrap();
        assert!(matches!(event, Event::Stopped(body) if body["reason"] == "breakpoint"));

        let stack = client
            .request("stackTrace", json!({"threadId": 1}))
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(stack["stackFrames"][0]["name"], "main");
        let scopes = client
            .request("scopes", json!({"frameId": 7}))
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(scopes["scopes"][0]["variablesReference"], 11);
        let variables = client
            .request("variables", json!({"variablesReference": 11}))
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(variables["variables"][0]["value"], "42");

        client
            .request("disconnect", json!({"terminateDebuggee": true}))
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        drop(client);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}
