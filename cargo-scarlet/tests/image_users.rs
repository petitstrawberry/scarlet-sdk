use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn build(project: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cargo-scarlet"))
        .args([
            "image",
            "--no-build",
            "--kernel-elf",
            "Cargo.toml",
            "--project",
        ])
        .arg(project)
        .args(["--image", "rootfs"])
        .output()
        .unwrap()
}

fn debugfs(image: &Path, query: &str) -> String {
    let output = Command::new("debugfs")
        .arg("-R")
        .arg(query)
        .arg(image)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn image_accounts_are_private_owned_and_cache_sensitive() {
    if ["debugfs", "mke2fs"]
        .iter()
        .any(|tool| Command::new(tool).arg("-V").output().is_err())
    {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("bsp/.cargo")).unwrap();
    fs::write(
        root.join("bsp/.cargo/config.toml"),
        "[build]\ntarget='aarch64-unknown-none-elf'",
    )
    .unwrap();
    fs::create_dir_all(root.join("files/home/alice")).unwrap();
    fs::write(root.join("files/home/alice/welcome"), "hello").unwrap();
    fs::write(
        root.join("scarlet.toml"),
        r#"
schema_version=2
[project]
name="test"
[bsp]
path="bsp"
package="scarlet"
[bsp.kernel]
source={path="kernel"}
[images.rootfs]
format="ext2"
output="root.ext2"
[[images.rootfs.layers]]
kind="copy"
source="files"
to="/"
"#,
    )
    .unwrap();
    let config = r#"
[[images.rootfs.users]]
name="alice"
uid=1001
gid=1010
groups=[42]
password-hash-file="hash"
[[images.rootfs.users]]
name="bob"
uid=1002
"#;
    let hash = "$2b$12$L6Bc/AlTQHyd9liGgGEZyOFLPHNgyxeEPfgYfBCVxJ7JIlwxyVU3u";
    fs::write(root.join("scarlet.local.toml"), config).unwrap();
    fs::write(root.join("hash"), hash).unwrap();
    let first = build(root);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let image = root.join("root.ext2");
    assert_eq!(
        debugfs(&image, "cat /etc/scarlet-users"),
        format!("alice:1001:1010:42:{hash}\nbob:1002:1002::!\n")
    );
    for (path, mode, uid, gid) in [
        ("/etc/scarlet-users", "0600", "0", "0"),
        ("/etc", "0755", "0", "0"),
        ("/home/alice", "0700", "1001", "1010"),
        ("/home/alice/welcome", "0644", "1001", "1010"),
        ("/home/bob", "0700", "1002", "1002"),
    ] {
        let stat = debugfs(&image, &format!("stat {path}"));
        let fields = stat.split_whitespace().collect::<Vec<_>>();
        for pair in [["Mode:", mode], ["User:", uid], ["Group:", gid]] {
            assert!(fields.windows(2).any(|w| w == pair), "{path}: {stat}");
        }
    }
    assert!(
        !fs::read_to_string(root.join("scarlet.lock"))
            .unwrap()
            .contains(hash)
    );
    assert!(String::from_utf8_lossy(&build(root).stderr).contains("unchanged"));
    let other_hash = hash.replace("FLPH", "GLPH");
    fs::write(root.join("hash"), &other_hash).unwrap();
    assert!(build(root).status.success());
    let records = debugfs(&image, "cat /etc/scarlet-users");
    assert!(records.contains(&other_hash));
    fs::write(
        root.join("scarlet.local.toml"),
        config.replace("uid=1002", "uid=1001"),
    )
    .unwrap();
    assert!(!build(root).status.success());
    assert_eq!(debugfs(&image, "cat /etc/scarlet-users"), records);
}
