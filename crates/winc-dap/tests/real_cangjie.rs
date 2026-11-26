//! Real `lldb-vscode` + Cangjie SDK verification.
//!
//! Ignored by default: needs a Cangjie SDK, `cjpm`, and the Python 3.11 runtime that
//! the SDK's LLDB 15 links against. Run with:
//!
//! `cargo test -p winc-dap --test real_cangjie -- --ignored --nocapture`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::json;
use winc_dap::{Client, Event, TcpConfig};

fn sdk_root() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("CANGJIE_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(root));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let base = PathBuf::from(home).join(".cjv/toolchains");
    let mut roots: Vec<PathBuf> = std::fs::read_dir(base)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.join("tools/bin/cjpm").is_file()
                && path.join("third_party/llvm/bin/lldb-vscode").is_file()
                && path
                    .join("runtime/lib/linux_x86_64_cjnative/libcangjie-runtime.so")
                    .is_file()
        })
        .collect();
    roots.sort();
    roots
        .iter()
        .rev()
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("sts"))
        })
        .or_else(|| roots.last())
        .cloned()
}

fn python_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("WINC_PYTHON311_HOME").filter(|value| !value.is_empty()) {
        let home = PathBuf::from(home);
        return home
            .join("lib/libpython3.11.so.1.0")
            .is_file()
            .then_some(home);
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
                    && path.join("lib/libpython3.11.so.1.0").is_file()
            })
    };
    find(PathBuf::from(&home).join(".local/share/cangjie"))
        .or_else(|| find(PathBuf::from(home).join(".local/share/winc")))
}

fn join_paths(paths: impl IntoIterator<Item = PathBuf>) -> String {
    std::env::join_paths(paths).unwrap().into_string().unwrap()
}

fn sdk_library_path(root: &Path, python: Option<&Path>) -> String {
    let mut paths = vec![
        root.join("third_party/llvm/lib"),
        root.join("tools/lib"),
        root.join("runtime/lib/linux_x86_64_cjnative"),
    ];
    if let Some(python) = python {
        paths.push(python.join("lib"));
    }
    join_paths(paths)
}

#[test]
#[ignore = "requires the Cangjie SDK and its Python 3.11 runtime"]
fn debugs_a_real_cangjie_program() {
    let root = sdk_root().expect("Cangjie SDK with lldb-vscode");
    let cjpm = root.join("tools/bin/cjpm");
    assert!(cjpm.is_file(), "{}", cjpm.display());
    let adapter = root.join("third_party/llvm/bin/lldb-vscode");
    let python = python_home().expect("Python 3.11 runtime for LLDB (WINC_PYTHON311_HOME)");

    let project = std::env::temp_dir().join(format!("winc-real-dap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&project);
    std::fs::create_dir_all(&project).unwrap();
    let mut tool_dirs = vec![root.join("tools/bin"), root.join("bin")];
    if let Some(path) = std::env::var_os("PATH") {
        tool_dirs.extend(std::env::split_paths(&path));
    }
    let tool_path = join_paths(tool_dirs);
    let status = Command::new(&cjpm)
        .arg("init")
        .arg("--name")
        .arg("debugdemo")
        .env("CANGJIE_HOME", &root)
        .env("PATH", &tool_path)
        .env("LD_LIBRARY_PATH", sdk_library_path(&root, None))
        .current_dir(&project)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::write(
        project.join("src/main.cj"),
        "package debugdemo\n\
         \n\
         func add(a: Int64, b: Int64): Int64 {\n\
         \x20   let sum = a + b\n\
         \x20   return sum\n\
         }\n\
         \n\
         main(): Unit {\n\
         \x20   let value = add(40, 2)\n\
         \x20   println(\"value=${value}\")\n\
         }\n",
    )
    .unwrap();
    let status = Command::new(&cjpm)
        .args(["build", "-g"])
        .env("CANGJIE_HOME", &root)
        .env("PATH", &tool_path)
        .env("LD_LIBRARY_PATH", sdk_library_path(&root, None))
        .current_dir(&project)
        .status()
        .unwrap();
    assert!(status.success());
    let source = project.join("src/main.cj");
    let program = project.join("target/debug/bin/main");
    assert!(program.is_file(), "{}", program.display());

    let mut config = TcpConfig::new(&adapter);
    config.cwd = Some(project.clone());
    config.env = vec![
        (
            "LD_LIBRARY_PATH".to_owned(),
            sdk_library_path(&root, Some(&python)),
        ),
        (
            "PYTHONHOME".to_owned(),
            python.to_string_lossy().into_owned(),
        ),
    ];
    let mut client = Client::connect(config).unwrap().client;
    let init = client.initialize(json!({})).unwrap();
    assert!(
        init["supportsConfigurationDoneRequest"]
            .as_bool()
            .unwrap_or(false)
    );
    client.initialized().unwrap();
    client
        .request(
            "launch",
            json!({
                "program": program,
                "cwd": project,
                "stopOnEntry": false,
                "sourceLanguages": ["cangjie"],
                "terminal": "console",
            }),
        )
        .unwrap()
        .recv_timeout(Duration::from_secs(20))
        .unwrap();
    let breakpoints = client
        .request(
            "setBreakpoints",
            json!({
                "source": {"name": "main.cj", "path": source},
                "breakpoints": [{"line": 10}],
            }),
        )
        .unwrap()
        .recv_timeout(Duration::from_secs(20))
        .unwrap();
    assert_eq!(breakpoints["breakpoints"][0]["verified"], true);
    client
        .request("configurationDone", json!({}))
        .unwrap()
        .recv_timeout(Duration::from_secs(20))
        .unwrap();

    let stopped = loop {
        match client
            .next_event(Duration::from_secs(30))
            .expect("stopped event")
        {
            Event::Stopped(body) => break body,
            Event::Output(_) | Event::Protocol(_) | Event::Other(_) => continue,
            other => panic!("unexpected event: {other:?}"),
        }
    };
    assert_eq!(stopped["reason"], "breakpoint");
    let thread = stopped["threadId"].as_i64().unwrap();
    let stack = client
        .request(
            "stackTrace",
            json!({"threadId": thread, "startFrame": 0, "levels": 10}),
        )
        .unwrap()
        .recv_timeout(Duration::from_secs(20))
        .unwrap();
    let frame = &stack["stackFrames"][0];
    assert_eq!(frame["line"], 10);
    assert!(frame["name"].as_str().unwrap_or_default().contains("main"));
    let scopes = client
        .request("scopes", json!({"frameId": frame["id"]}))
        .unwrap()
        .recv_timeout(Duration::from_secs(20))
        .unwrap();
    let reference = scopes["scopes"][0]["variablesReference"].as_i64().unwrap();
    let variables = client
        .request("variables", json!({"variablesReference": reference}))
        .unwrap()
        .recv_timeout(Duration::from_secs(20))
        .unwrap();
    assert!(
        variables["variables"]
            .as_array()
            .unwrap()
            .iter()
            .any(|variable| variable["name"] == "value" && variable["value"] == "42"),
        "{variables}"
    );
    client
        .request("continue", json!({"threadId": thread}))
        .unwrap()
        .recv_timeout(Duration::from_secs(20))
        .unwrap();
    loop {
        match client
            .next_event(Duration::from_secs(20))
            .expect("terminated event")
        {
            Event::Terminated(_) => break,
            Event::Output(_) | Event::Protocol(_) | Event::Other(_) => continue,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    client
        .request("disconnect", json!({"terminateDebuggee": true}))
        .unwrap()
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    drop(client);
    let _ = std::fs::remove_dir_all(&project);
}
