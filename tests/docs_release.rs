//! The installation guide's upgrade commands name the current release, so the release
//! checklist (bump them together with the crate version) cannot be skipped
//! unnoticed.

#[test]
fn upgrade_commands_name_the_current_release() {
    // A Windows checkout may carry CRLF line endings.
    let guide =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/installation.md"))
            .unwrap()
            .replace("\r\n", "\n");
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let upgrade = guide
        .split("\n## Upgrade\n")
        .nth(1)
        .expect("the installation guide has an Upgrade section");
    let upgrade = upgrade.split("\n## ").next().unwrap();
    for needle in [
        format!("VERSION={version}"),
        format!("--tag {version}"),
        format!("ccsw-{version}-x86_64-pc-windows-msvc.zip"),
    ] {
        assert!(
            upgrade.contains(&needle),
            "the Upgrade section names the current release: missing {needle:?}"
        );
    }
}
