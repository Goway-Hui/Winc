//! 仓颉项目创建、文件模板与工具链命令；语言服务状态由 [`super::lsp`] 管理。
#![allow(
    clippy::disallowed_methods,
    reason = "SDK discovery, cjpm init and generated project files only touch local paths"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use super::App;
use winc_config::RunConfig;
use winc_i18n::t;

/// 仓颉项目的常用命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CangjieCommand {
    /// 构建项目。
    Build,
    /// 测试项目。
    Test,
    /// 运行项目。
    Run,
}

impl CangjieCommand {
    fn name(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Test => "test",
            Self::Run => "run",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Build => t!("cmd-cangjie-build"),
            Self::Test => t!("cmd-cangjie-test"),
            Self::Run => t!("cmd-cangjie-run"),
        }
    }
}

/// 返回仓颉 SDK 根目录：配置的路径、`CANGJIE_HOME`、cjv 默认工具链，最后取最新的
/// `sts-*` 或任意含编译器与调试器的工具链。
pub(crate) fn sdk_root(configured: Option<&str>) -> Option<PathBuf> {
    if let Some(root) = configured
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
    {
        return Some(root);
    }
    if let Some(root) = std::env::var_os("CANGJIE_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(root));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let cjv = PathBuf::from(home).join(".cjv");
    if let Ok(settings) = std::fs::read_to_string(cjv.join("settings.toml"))
        && let Some(name) = settings.lines().find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "default_toolchain").then(|| value.trim().trim_matches(['"', '\'']))
        })
    {
        let root = cjv.join("toolchains").join(name);
        if root.is_dir() {
            return Some(root);
        }
    }
    let mut roots: Vec<PathBuf> = std::fs::read_dir(cjv.join("toolchains"))
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.join("tools/bin/cjpm").is_file()
                || path.join("bin/cjc").is_file()
                || path.join("third_party/llvm/bin/lldb-vscode").is_file()
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

/// 查找 `cjpm`：`PATH`、`CANGJIE_HOME/tools/bin`、`~/.cjv/bin` 与 `toolchains/*/tools/bin`。
pub(crate) fn find_cjpm(configured_sdk: Option<&str>) -> Option<PathBuf> {
    let exe = if cfg!(windows) { "cjpm.exe" } else { "cjpm" };
    if let Some(root) = configured_sdk
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        let root = PathBuf::from(root);
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
    for entry in std::fs::read_dir(cjv.join("toolchains")).ok()?.flatten() {
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

/// 校验仓颉项目名：ASCII 字母、数字、下划线、连字符，且不以数字开头。
pub(crate) fn valid_project_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// 安全创建仓颉项目；优先调用 `cjpm init`，不可用时写最小项目骨架。
///
/// 父目录必须已存在，目标目录必须不存在或为空；已有 `cjpm.toml` 永不覆盖。`cjpm init` 失败时
/// 清理本次创建的目标目录，不留下半成品。
pub(crate) fn create_project(parent: &Path, name: &str) -> Result<PathBuf, String> {
    if !valid_project_name(name) {
        return Err(t!("cangjie-project-invalid-name").to_owned());
    }
    if !parent.is_dir() {
        return Err(t!("cangjie-project-invalid-location").to_owned());
    }
    let root = parent.join(name);
    if root.join("cjpm.toml").exists() {
        return Err(t!("cangjie-project-exists").to_owned());
    }
    let empty = match std::fs::read_dir(&root) {
        Ok(mut entries) => entries.next().is_none(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.to_string()),
    };
    if !empty {
        return Err(t!("cangjie-project-exists").to_owned());
    }
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    if let Some(cjpm) = find_cjpm(None) {
        match Command::new(cjpm)
            .arg("init")
            .arg("--name")
            .arg(name)
            .current_dir(&root)
            .status()
        {
            Ok(status) if status.success() && root.join("cjpm.toml").is_file() => {
                return Ok(root);
            }
            Ok(status) => {
                let _ = std::fs::remove_dir_all(&root);
                return Err(format!("cjpm init exited with {status}"));
            }
            Err(error) => {
                let _ = std::fs::remove_dir_all(&root);
                return Err(error.to_string());
            }
        }
    }
    let src = root.join("src");
    std::fs::create_dir_all(&src).map_err(|error| error.to_string())?;
    let manifest = format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n[dependencies]\n"
    );
    std::fs::write(root.join("cjpm.toml"), manifest).map_err(|error| error.to_string())?;
    std::fs::write(
        src.join("main.cj"),
        "main(): Unit {\n    println(\"Hello, Cangjie!\")\n}\n",
    )
    .map_err(|error| error.to_string())?;
    Ok(root)
}

impl App {
    /// 新建一个使用仓颉语法的未命名文件。
    pub(in crate::app) fn new_cangjie_file(&mut self) {
        self.new_file();
        let language = winc_syntax::languages()
            .iter()
            .copied()
            .find(|language| language.id == "cangjie");
        if let (Some(language), Some(doc)) = (language, self.editor.active_mut()) {
            doc.choose_language(language);
        }
        self.request_redraw();
    }

    /// 打开仓颉项目创建流程：先选父目录，再输入项目名。
    pub(in crate::app) fn ask_new_cangjie_project(&mut self) {
        self.pick(super::workspace::files::Pick::NewCangjieProject);
    }

    /// 父目录选好后收集项目名。
    pub(in crate::app) fn prompt_cangjie_project(&mut self, parent: PathBuf) {
        let mut field = crate::app::TextField::default();
        field.set_text("cangjie-app");
        self.open_prompt(crate::app::PromptKind::CangjieProject { parent }, field);
    }

    /// 创建项目并在当前窗口打开；失败不改变当前工作区。
    pub(in crate::app) fn create_cangjie_project(&mut self, parent: &Path, name: &str) {
        match create_project(parent, name.trim()) {
            Ok(root) => {
                self.flash(t!("cangjie-project-created", name = name.trim()));
                self.open_path(&root);
            }
            Err(error) => self.notify(
                crate::app::lifecycle::notice::NoticeKind::Error,
                t!("cangjie-project-create-failed", error = error),
            ),
        }
    }

    /// 当前窗口打开的目录是否是本机的仓颉项目。
    pub(crate) fn cangjie_project_open(&self) -> bool {
        self.editor
            .root()
            .filter(|root| {
                !winc_backend::is_remote(root) && winc_editor::wsl::locate(root).is_none()
            })
            .is_some_and(|root| root.join("cjpm.toml").is_file())
    }

    /// 通过现有运行配置信任链执行仓颉 build/test/run。
    pub(in crate::app) fn run_cangjie_command(&mut self, command: CangjieCommand) {
        let Some(root) = self.editor.root().filter(|root| {
            !winc_backend::is_remote(root)
                && winc_editor::wsl::locate(root).is_none()
                && root.join("cjpm.toml").is_file()
        }) else {
            self.flash(t!("cangjie-project-cjpm-missing"));
            return;
        };
        let Some(cjpm) = find_cjpm(self.config.cangjie_sdk_path.as_deref()) else {
            self.flash(t!("cangjie-project-cjpm-missing"));
            return;
        };
        let config = RunConfig {
            name: format!("Cangjie: {}", command.title()),
            command: cjpm.to_string_lossy().into_owned(),
            args: vec![command.name().to_owned()],
            cwd: Some(root.to_string_lossy().into_owned()),
            env: Vec::new(),
        };
        let trusted = self
            .state
            .command_approved(&winc_config::run_configs_path(root), &config.approval());
        if trusted || self.window.handle.is_none() {
            self.start_run(config);
            return;
        }
        let message = t!("terminal-run-confirm", name = config.name.as_str());
        let detail = format!(
            "{}\n\n{}",
            t!("terminal-run-confirm-detected"),
            t!(
                "terminal-run-confirm-command",
                command = format!("{} {}", cjpm.display(), command.name())
            )
        );
        let buttons = vec![
            crate::app::AlertButton::new(t!("terminal-run-confirm-action")),
            crate::app::AlertButton::cancel(t!("common-cancel")),
        ];
        self.show_alert(&message, &detail, buttons, None, move |index, _| {
            (index == 0).then(|| crate::app::UserEvent::RunApproved(Box::new(config.clone())))
        });
    }
}

#[cfg(test)]
mod tests;
