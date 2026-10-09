//! The README's upgrade commands name the current release, so the release
//! checklist (bump them together with the crate version) cannot be skipped
//! unnoticed.

#[test]
fn readme_upgrade_commands_name_the_current_release() {
    let readme =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).unwrap();
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let upgrade = readme
        .split("\n## Upgrade\n")
        .nth(1)
        .expect("README has an Upgrade section");
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
