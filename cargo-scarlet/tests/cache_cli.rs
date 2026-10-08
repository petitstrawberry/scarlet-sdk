use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

struct Project(PathBuf);
static NEXT_PROJECT: AtomicU64 = AtomicU64::new(0);

impl Project {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "scarlet-maintenance-cli-{}-{}-{}",
            std::process::id(),
            NEXT_PROJECT.fetch_add(1, Ordering::Relaxed),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        // Maintenance must also work when a project's manifest needs repair.
        fs::write(path.join("scarlet.toml"), "invalid TOML [").unwrap();
        Self(path)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-scarlet"));
        command.current_dir(&self.0);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn write(&self, path: &str) -> PathBuf {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![1; 8192]).unwrap();
        path
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn maintenance_cli_supports_cargo_dispatch_and_project_options() {
    let project = Project::new();
    let target = project.write(".scarlet/cache/target/0123456789abcdef/debug/binary");
    let image = project.write(".scarlet/images/rootfs.img");
    let list = success(project.run(&["scarlet", "cache", "list"]));
    assert!(list.contains("Cargo build outputs"));
    assert!(list.contains("mtime"));
    let project_arg = project.0.to_str().unwrap();
    // Accept --project both before and after the cache subcommand.
    success(project.run(&["cache", "--project", project_arg, "list"]));
    success(project.run(&["cache", "list", "--project", project_arg]));
    assert!(!project.run(&["cache", "prune"]).status.success());
    let preview = success(project.run(&[
        "cache",
        "prune",
        "--max-age",
        "30",
        "--max-size",
        "0",
        "--dry-run",
    ]));
    assert!(preview.contains("Would remove 1 target cache"));
    assert!(target.exists());
    success(project.run(&["cache", "prune", "--max-size", "0B"]));
    assert!(!target.exists());
    assert!(image.exists());
    assert!(success(project.run(&["clean", "--dry-run"])).contains("Would remove"));
    assert!(image.exists());
    success(project.run(&["scarlet", "clean", "--project", project_arg]));
    assert!(!project.0.join(".scarlet").exists());
    assert!(project.0.join("scarlet.toml").exists());
    assert!(project.0.join(".scarlet-operation.lock").exists());
    assert!(success(project.run(&["clean"])).contains("Nothing to clean"));
}

#[test]
fn maintenance_requires_project_marker_before_touching_anything() {
    let project = Project::new();
    let image = project.write(".scarlet/images/keep.img");
    fs::remove_file(project.0.join("scarlet.toml")).unwrap();
    for args in [
        vec!["clean"],
        vec!["cache", "list"],
        vec!["cache", "prune", "--max-size", "0"],
    ] {
        let output = project.run(&args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("scarlet.toml is missing"));
    }
    assert!(image.exists());
    assert!(!project.0.join(".scarlet-operation.lock").exists());
}

#[cfg(unix)]
#[test]
fn maintenance_is_blocked_for_the_entire_runner_lifetime() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;
    let project = Project::new();
    fs::create_dir(project.0.join("kernel")).unwrap();
    fs::write(
        project.0.join("scarlet.toml"),
        r#"
schema_version = 2
[project]
name = "test"
[kernel]
package = "scarlet"
source = "kernel"
target_json = "aarch64-unknown-none.json"
[runner]
command = "runner.sh"
"#,
    )
    .unwrap();
    let script = project.0.join("runner.sh");
    let shell = success(
        Command::new("sh")
            .args(["-c", "command -v sh"])
            .output()
            .unwrap(),
    );
    fs::write(
        &script,
        format!(
            "#!{}\necho RUNNER_READY\nread ignored\nexit 0\n",
            shell.trim()
        ),
    )
    .unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
    let image = project.write(".scarlet/images/in-use.img");
    let mut runner = project
        .command()
        .args(["run", "--project", ".", "--no-image"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(runner.stdout.take().unwrap());
    let mut line = String::new();
    assert_ne!(
        stdout.read_line(&mut line).unwrap(),
        0,
        "runner exited early"
    );
    assert!(line.contains("RUNNER_READY"));
    let results: Vec<_> = [
        vec!["clean"],
        vec!["clean", "--dry-run"],
        vec!["cache", "prune", "--max-size", "0"],
    ]
    .iter()
    .map(|args| project.run(args))
    .collect();
    success(project.run(&["cache", "list"]));
    drop(runner.stdin.take());
    assert!(runner.wait().unwrap().success());
    for output in results {
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("in use"));
    }
    assert!(image.exists());
    success(project.run(&["clean"]));
    assert!(!image.exists());
}

#[cfg(unix)]
#[test]
fn app_build_locks_project_and_standalone_recipe_cache_roots() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;

    for standalone in [false, true] {
        let project = Project::new();
        let source = project.0.join("source");
        fs::create_dir(&source).unwrap();
        let cache_project = if standalone {
            fs::remove_file(project.0.join("scarlet.toml")).unwrap();
            fs::write(source.join("scarlet.toml"), "invalid TOML [").unwrap();
            &source
        } else {
            &project.0
        };
        let image = cache_project.join(".scarlet/images/keep.img");
        fs::create_dir_all(image.parent().unwrap()).unwrap();
        fs::write(&image, "guest data").unwrap();
        fs::write(
            source.join("app.toml"),
            r#"
[app]
id = "org.test.player"
slug = "player"
name = "Player"
exec = "bin/player"
[build]
kind = "script"
source = "wait.sh"
"#,
        )
        .unwrap();
        let mut elf = [0u8; 64];
        elf[..8].copy_from_slice(b"\x7fELF\x02\x01\x01\x53");
        elf[16..18].copy_from_slice(&2u16.to_le_bytes());
        elf[18..20].copy_from_slice(&183u16.to_le_bytes());
        fs::write(source.join("player"), elf).unwrap();
        let script = source.join("wait.sh");
        let shell = success(
            Command::new("sh")
                .args(["-c", "command -v sh"])
                .output()
                .unwrap(),
        );
        fs::write(
            &script,
            format!(
                "#!{}\necho APP_BUILD_READY\nread ignored\ncp player \"$2\"\nchmod +x \"$2\"\n",
                shell.trim()
            ),
        )
        .unwrap();
        fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
        let mut builder = project
            .command()
            .args([
                "app",
                "build",
                "--source",
                "source",
                "--target",
                "aarch64-unknown-scarlet",
                "--output",
                "player.app",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(builder.stdout.take().unwrap());
        let mut line = String::new();
        assert_ne!(
            stdout.read_line(&mut line).unwrap(),
            0,
            "app builder exited early"
        );
        assert!(line.contains("APP_BUILD_READY"));
        let protected = cache_project.to_str().unwrap();
        let results = [
            project.run(&["clean", "--project", protected]),
            project.run(&["cache", "prune", "--project", protected, "--max-size", "0"]),
        ];
        success(project.run(&["cache", "list", "--project", protected]));
        drop(builder.stdin.take());
        assert!(builder.wait().unwrap().success());
        for output in results {
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("in use"));
        }
        assert!(image.exists());
        success(project.run(&["clean", "--project", protected]));
        assert!(!image.exists());
    }
}
