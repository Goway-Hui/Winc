//! 调试断点与命令的离屏测试。

use super::*;
use crate::app::debug::DebugStatus;
use crate::keymap::Action;

#[test]
fn breakpoint_toggles_at_the_caret_and_is_remembered() {
    let dir = sample_folder("debug-breakpoint");
    let path = dir.join("src/main.rs");
    let mut app = app_with(&path, false);
    let active = app.editor.active().unwrap();
    let line = 1;
    let offset = active.text().line_start(line);
    let id = active.id;
    app.select(offset, offset);

    app.run_action(Action::ToggleBreakpoint);
    assert_eq!(app.debug.breakpoints.len(), 1);
    let breakpoint = &app.debug.breakpoints[0];
    assert_eq!(
        (
            breakpoint.document,
            breakpoint.path.as_path(),
            breakpoint.line
        ),
        (id, path.as_path(), line)
    );
    assert!(breakpoint.enabled);

    let root = app.editor.root().unwrap().to_path_buf();
    let stored = app.state.debug_breakpoints(&root);
    assert_eq!(
        stored.get(&path).map(|rows| rows.as_slice()),
        Some(&[(line, 0, true)][..])
    );

    // 标记画在行号栏上，颜色使用错误色。
    paint(&mut app);
    assert!(
        app.debug
            .breakpoints
            .iter()
            .any(|bp| bp.line == line && bp.enabled)
    );

    app.run_action(Action::ToggleBreakpoint);
    assert!(app.debug.breakpoints.is_empty());
    assert!(app.state.debug_breakpoints(&root).is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn debug_commands_are_unavailable_without_a_cangjie_project() {
    let dir = sample_folder("debug-commands");
    let mut app = app_with(&dir.join("src/main.rs"), false);
    assert_eq!(app.debug.status, DebugStatus::Idle);
    assert!(!app.action_available(Action::DebugStart));
    app.run_action(Action::DebugStart);
    assert_eq!(app.debug.status, DebugStatus::Idle);
    let _ = std::fs::remove_dir_all(&dir);
}
