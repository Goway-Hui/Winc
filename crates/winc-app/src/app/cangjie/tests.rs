//! 仓颉项目与工具链测试。

use super::*;

#[test]
fn project_name_validation_rejects_traversal_and_bad_first_character() {
    assert!(valid_project_name("hello-cangjie_2"));
    for name in ["", "2app", "../x", "a/b", "a b", ".hidden", "应用"] {
        assert!(!valid_project_name(name), "{name}");
    }
}

#[test]
fn fallback_project_creation_writes_manifest_and_main_without_overwriting() {
    let parent = std::env::temp_dir().join(format!("winc-cangjie-project-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&parent);
    std::fs::create_dir_all(&parent).unwrap();
    let root = create_project(&parent, "demo").unwrap();
    assert_eq!(root, parent.join("demo"));
    assert!(root.join("cjpm.toml").is_file());
    assert!(root.join("src/main.cj").is_file());
    assert!(create_project(&parent, "demo").is_err());
    std::fs::write(root.join("keep.txt"), "keep").unwrap();
    assert!(create_project(&parent, "demo").is_err());
    assert_eq!(
        std::fs::read_to_string(root.join("keep.txt")).unwrap(),
        "keep"
    );
    let _ = std::fs::remove_dir_all(&parent);
}

#[test]
fn fallback_creation_rejects_nonempty_directory() {
    let parent = std::env::temp_dir().join(format!("winc-cangjie-nonempty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&parent);
    std::fs::create_dir_all(parent.join("demo")).unwrap();
    std::fs::write(parent.join("demo/data"), "data").unwrap();
    assert!(create_project(&parent, "demo").is_err());
    assert_eq!(
        std::fs::read_to_string(parent.join("demo/data")).unwrap(),
        "data"
    );
    let _ = std::fs::remove_dir_all(&parent);
}
