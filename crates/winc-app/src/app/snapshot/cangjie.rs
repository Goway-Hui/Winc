//! 仓颉项目界面测试。

use super::*;
use crate::app::StatusItem;

#[test]
fn cangjie_project_shows_build_test_run_and_debug_in_the_status_bar() {
    let dir = sample_folder("cangjie-actions");
    std::fs::write(dir.join("cjpm.toml"), "[package]\nname = \"demo\"\n").unwrap();
    let mut app = app_with(&dir, false);
    paint(&mut app);

    for item in [
        StatusItem::CangjieBuild,
        StatusItem::CangjieTest,
        StatusItem::CangjieRun,
        StatusItem::CangjieDebug,
    ] {
        assert!(
            app.hits.status.iter().any(|(_, hit)| *hit == item),
            "missing {item:?}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cangjie_project_shows_run_controls_in_the_top_bar() {
    let dir = sample_folder("cangjie-top-run");
    std::fs::write(dir.join("cjpm.toml"), "[package]\nname = \"demo\"\n").unwrap();
    let mut app = app_with(&dir, false);
    app.reload_run_configs();
    paint(&mut app);

    let menu = app.hits.top_run_menu.expect("run menu");
    let play = app.hits.top_run_play.expect("run button");
    assert!(menu.right() < play.x);
    assert!(play.right() <= app.layout.tabs.right());
    assert!(app.tabs.new_tab.right() <= menu.x);
    assert_eq!(app.run.current().map(|c| c.name.as_str()), Some("run"));

    click(&mut app, (menu.x + menu.w / 2.0, menu.y + menu.h / 2.0));
    assert_eq!(
        app.overlays.menu_button,
        Some(crate::app::MenuButton::TopRun)
    );
    assert!(app.overlays.context_menu.is_some());

    let _ = std::fs::remove_dir_all(&dir);
}
