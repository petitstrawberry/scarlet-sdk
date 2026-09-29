//! Project-local cache accounting and explicit maintenance. Never follows symlinks.

#[cfg(unix)]
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const LOCK_FILE: &str = ".scarlet-operation.lock";
const USAGE_FILE: &str = ".scarlet-cache-use.json";

/// Kept outside .scarlet so removing the cache cannot replace a held lock inode.
/// SDK work holds shared locks through child execution; deletion is exclusive.
pub(crate) struct ProjectLock {
    _file: File,
}

impl ProjectLock {
    pub(crate) fn activity(project: &Path) -> Result<Self, String> {
        Self::acquire(project, false)
    }

    pub(crate) fn maintenance(project: &Path) -> Result<Self, String> {
        Self::acquire(project, true)
    }

    fn acquire(project: &Path, exclusive: bool) -> Result<Self, String> {
        let path = project.join(LOCK_FILE);
        if let Some(metadata) = metadata_if_present(&path)?
            && !metadata.is_file()
        {
            return Err(format!("refusing non-regular lock file {}", path.display()));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| format!("failed to open {}: {e}", path.display()))?;
        let result = if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };
        result.map_err(|e| match e {
            TryLockError::WouldBlock => format!(
                "project {} is in use by another cargo-scarlet command; retry after it exits",
                project.display()
            ),
            TryLockError::Error(e) => format!("failed to lock {}: {e}", path.display()),
        })?;
        Ok(Self { _file: file })
    }
}

#[derive(Serialize, Deserialize)]
struct Usage {
    source: PathBuf,
    last_used: u64,
}

/// Called before and after Cargo, including failed builds. Failure to record
/// accounting information should not make an otherwise valid build fail.
pub(crate) fn record_use(target: &Path, source: &Path) {
    let result = (|| -> Result<(), String> {
        fs::create_dir_all(target).map_err(|e| e.to_string())?;
        let usage = Usage {
            source: fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf()),
            last_used: timestamp(SystemTime::now()),
        };
        let path = target.join(USAGE_FILE);
        // Replace the file atomically, without writing through an existing symlink.
        let temporary = target.join(format!(
            "{USAGE_FILE}.{}.{}.tmp",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        let result = serde_json::to_writer(&mut file, &usage)
            .map_err(|e| e.to_string())
            .and_then(|()| fs::rename(&temporary, &path).map_err(|e| e.to_string()));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    })();
    if let Err(error) = result {
        eprintln!(
            "cargo-scarlet: could not record cache use for {}: {error}",
            target.display()
        );
    }
}

#[derive(Debug)]
struct Entry {
    path: PathBuf,
    purpose: &'static str,
    bytes: u64,
    last_used: u64,
    recorded: bool,
    source: Option<PathBuf>,
    prunable: bool,
}

#[derive(Default)]
struct Stats {
    bytes: u64,
    modified: u64,
    #[cfg(unix)]
    inodes: HashSet<(u64, u64)>,
}

fn timestamp(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn metadata_if_present(path: &Path) -> Result<Option<fs::Metadata>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("failed to inspect {}: {e}", path.display())),
    }
}

fn children(path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = fs::read_dir(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    paths.sort();
    Ok(paths)
}

fn measure(path: &Path, stats: &mut Stats) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("failed to inspect {}: {e}", path.display()))?;
    stats.modified = stats
        .modified
        .max(timestamp(metadata.modified().map_err(|e| {
            format!(
                "failed to read modification time for {}: {e}",
                path.display()
            )
        })?));
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if stats.inodes.insert((metadata.dev(), metadata.ino())) {
            stats.bytes = stats
                .bytes
                .saturating_add(metadata.blocks().saturating_mul(512));
        }
    }
    #[cfg(not(unix))]
    {
        stats.bytes = stats.bytes.saturating_add(metadata.len());
    }
    if metadata.is_dir() {
        for child in children(path)? {
            measure(&child, stats)?;
        }
    }
    Ok(())
}

fn entry(path: PathBuf, purpose: &'static str, prunable: bool) -> Result<Entry, String> {
    let mut stats = Stats::default();
    measure(&path, &mut stats)?;
    let usage_path = path.join(USAGE_FILE);
    let usage: Option<Usage> =
        if prunable && metadata_if_present(&usage_path)?.is_some_and(|m| m.is_file()) {
            // Old/interrupted/corrupt metadata falls back to filesystem timestamps.
            fs::read(&usage_path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        } else {
            None
        };
    Ok(Entry {
        path,
        purpose,
        bytes: stats.bytes,
        last_used: usage
            .as_ref()
            .map_or(stats.modified, |u| u.last_used.max(stats.modified)),
        recorded: usage
            .as_ref()
            .is_some_and(|u| u.last_used >= stats.modified),
        source: usage.map(|u| u.source),
        prunable,
    })
}

fn real_directory(path: &Path) -> Result<bool, String> {
    Ok(metadata_if_present(path)?.is_some_and(|metadata| metadata.is_dir()))
}

fn cache_root(project: &Path) -> Result<Option<PathBuf>, String> {
    let root = project.join(".scarlet");
    match metadata_if_present(&root)? {
        None => Ok(None),
        Some(metadata) if metadata.is_dir() => Ok(Some(root)),
        Some(_) => Err(format!(
            "refusing to operate on {}: expected a directory, not a file or symlink",
            root.display()
        )),
    }
}

fn inventory(project: &Path, targets_only: bool) -> Result<Vec<Entry>, String> {
    let Some(root) = cache_root(project)? else {
        return Ok(Vec::new());
    };
    let mut entries = Vec::new();
    for path in children(&root)? {
        if path.file_name().is_some_and(|name| name == "cache") && real_directory(&path)? {
            for path in children(&path)? {
                let name = path.file_name().unwrap().to_string_lossy();
                if name == "target" && real_directory(&path)? {
                    for target in children(&path)? {
                        let name = target.file_name().unwrap().to_string_lossy();
                        let prunable = name.len() == 16
                            && name
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                            && real_directory(&target)?;
                        entries.push(entry(
                            target,
                            if prunable {
                                "Cargo build outputs"
                            } else {
                                "unmanaged target entry"
                            },
                            prunable,
                        )?);
                    }
                } else if !targets_only {
                    let purpose = match name.as_ref() {
                        "git" => "source checkouts",
                        "files" => "downloaded inputs",
                        "cargo-home" => "Cargo registry / Git inputs",
                        _ => "other cache data",
                    };
                    entries.push(entry(path, purpose, false)?);
                }
            }
        } else if !targets_only {
            let purpose = match path.file_name().unwrap().to_str() {
                Some("images") => "images (may contain guest data)",
                Some("scarlet-modules") => "generated module workspace",
                _ => "other project artifacts",
            };
            entries.push(entry(path, purpose, false)?);
        }
    }
    entries.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.path.cmp(&b.path)));
    Ok(entries)
}

pub(crate) fn validate_project(project: &Path) -> Result<(), String> {
    if !project.join("scarlet.toml").is_file() {
        return Err(format!(
            "{} is not a Scarlet project (scarlet.toml is missing)",
            project.display()
        ));
    }
    Ok(())
}

fn human_size(bytes: u64) -> String {
    for (unit, scale) in [
        ("TiB", 1_u64 << 40),
        ("GiB", 1 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
    ] {
        if bytes >= scale {
            return format!("{:.1} {unit}", bytes as f64 / scale as f64);
        }
    }
    format!("{bytes} B")
}

pub(crate) fn parse_size(value: &str) -> Result<u64, String> {
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let amount: u64 = value[..split]
        .parse()
        .map_err(|_| "expected an integer size, e.g. 20GiB".to_string())?;
    let scale = match value[split..].to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kib" => 1 << 10,
        "mib" => 1 << 20,
        "gib" => 1 << 30,
        "tib" => 1_u64 << 40,
        "kb" => 1_000,
        "mb" => 1_000_000,
        "gb" => 1_000_000_000,
        "tb" => 1_000_000_000_000,
        _ => return Err("use B, KiB, MiB, GiB, TiB, KB, MB, GB or TB".to_string()),
    };
    amount
        .checked_mul(scale)
        .ok_or_else(|| "size is too large".to_string())
}

pub(crate) fn list(project: &Path) -> Result<(), String> {
    let entries = inventory(project, false)?;
    let now = timestamp(SystemTime::now());
    println!(
        "{:>12}  {:>20}  {:7}  PATH / PURPOSE",
        "DISK SIZE", "LAST USE / MODIFIED", "PRUNE"
    );
    for entry in &entries {
        let age = format!(
            "{}d ago ({})",
            now.saturating_sub(entry.last_used) / 86400,
            if entry.recorded { "used" } else { "mtime" }
        );
        println!(
            "{:>12}  {:>20}  {:7}  {} — {}",
            human_size(entry.bytes),
            age,
            if entry.prunable { "eligible" } else { "keep" },
            entry
                .path
                .strip_prefix(project)
                .unwrap_or(&entry.path)
                .display(),
            entry.purpose
        );
        if let Some(source) = &entry.source {
            println!(
                "                                               source: {}",
                source.display()
            );
        }
    }
    let total: u64 = entries.iter().map(|e| e.bytes).sum();
    let targets: u64 = entries.iter().filter(|e| e.prunable).map(|e| e.bytes).sum();
    println!(
        "Total: {}; prunable Cargo targets: {}",
        human_size(total),
        human_size(targets)
    );
    println!(
        "mtime is an estimate for caches without use records; sizes may differ from space freed."
    );
    Ok(())
}

fn plan_prune(
    entries: &[Entry],
    now: u64,
    max_age: Option<Duration>,
    max_size: Option<u64>,
) -> Vec<usize> {
    let mut ordered: Vec<usize> = (0..entries.len())
        .filter(|&i| entries[i].prunable)
        .collect();
    ordered.sort_by(|&a, &b| {
        entries[a]
            .last_used
            .cmp(&entries[b].last_used)
            .then(entries[a].path.cmp(&entries[b].path))
    });
    let mut remaining: u64 = ordered.iter().map(|&i| entries[i].bytes).sum();
    let mut selected = Vec::new();
    for i in ordered {
        let expired =
            max_age.is_some_and(|age| now.saturating_sub(entries[i].last_used) > age.as_secs());
        if expired || max_size.is_some_and(|limit| limit == 0 || remaining > limit) {
            selected.push(i);
            remaining = remaining.saturating_sub(entries[i].bytes);
        }
    }
    selected
}

/// Use the enclosing working tree's index, including staged additions. A cache
/// checkout's own index is deliberately not considered: clean includes sources.
fn ensure_untracked(project: &Path, path: &Path) -> Result<(), String> {
    if !project.ancestors().any(|p| p.join(".git").exists()) {
        return Ok(());
    }
    let relative = path.strip_prefix(project).map_err(|e| e.to_string())?;
    let output = Command::new("git")
        .arg("-C")
        .arg(project)
        .args(["--literal-pathspecs", "ls-files", "--cached", "-z", "--"])
        .arg(relative)
        .output()
        .map_err(|e| format!("failed to check Git tracking: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "failed to check Git tracking: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if !output.stdout.is_empty() {
        return Err(format!(
            "refusing to delete {}: contains Git-tracked files",
            path.display()
        ));
    }
    Ok(())
}

/// Source copies can contain read-only directories (e.g. from the Nix store).
/// Only change directory permissions, never a file that might be hard-linked.
fn make_directories_writable(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("failed to inspect {}: {e}", path.display()))?;
    if metadata.is_dir() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = metadata.permissions().mode();
            if mode & 0o700 != 0o700 {
                fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o700))
                    .map_err(|e| format!("failed to make {} writable: {e}", path.display()))?;
            }
        }
        for child in children(path)? {
            make_directories_writable(&child)?;
        }
    }
    Ok(())
}

fn remove_tree(path: &Path) -> Result<(), String> {
    if !real_directory(path)? {
        return Err(format!(
            "refusing to remove {}: expected a directory",
            path.display()
        ));
    }
    make_directories_writable(path)?;
    fs::remove_dir_all(path).map_err(|e| format!("failed to remove {}: {e}", path.display()))
}

pub(crate) fn prune(
    project: &Path,
    max_age_days: Option<u64>,
    max_size: Option<u64>,
    dry_run: bool,
) -> Result<(), String> {
    if max_age_days.is_none() && max_size.is_none() {
        return Err("cache prune requires --max-age <DAYS> and/or --max-size <SIZE>".to_string());
    }
    let max_age = max_age_days
        .map(|days| {
            days.checked_mul(86400)
                .map(Duration::from_secs)
                .ok_or("--max-age is too large")
        })
        .transpose()?;
    let entries = inventory(project, true)?;
    let selected = plan_prune(&entries, timestamp(SystemTime::now()), max_age, max_size);
    // Preflight the whole plan before making any deletions.
    for &i in &selected {
        ensure_untracked(project, &entries[i].path)?;
    }
    let mut bytes = 0_u64;
    for &i in &selected {
        let entry = &entries[i];
        println!(
            "{} {} ({})",
            if dry_run { "Would remove" } else { "Removing" },
            entry.path.display(),
            human_size(entry.bytes)
        );
        if !dry_run {
            remove_tree(&entry.path)?;
        }
        bytes = bytes.saturating_add(entry.bytes);
    }
    println!(
        "{} {} target cache(s), approximately {}",
        if dry_run { "Would remove" } else { "Removed" },
        selected.len(),
        human_size(bytes)
    );
    Ok(())
}

pub(crate) fn clean(project: &Path, dry_run: bool) -> Result<(), String> {
    let Some(root) = cache_root(project)? else {
        println!(
            "Nothing to clean: {} does not exist",
            project.join(".scarlet").display()
        );
        return Ok(());
    };
    ensure_untracked(project, &root)?;
    let mut stats = Stats::default();
    measure(&root, &mut stats)?;
    if dry_run {
        println!(
            "Would remove {} entirely (approximately {})",
            root.display(),
            human_size(stats.bytes)
        );
    } else {
        remove_tree(&root)?;
        println!(
            "Removed {} (approximately {})",
            root.display(),
            human_size(stats.bytes)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "cargo-scarlet-cache-{}-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("scarlet.toml"), "[project]\nname = 'test'\n").unwrap();
            Self(fs::canonicalize(path).unwrap())
        }

        fn write(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, vec![42; 8192]).unwrap();
            path
        }

        fn git(&self, args: &[&str]) {
            let output = Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = remove_tree(&self.0);
        }
    }

    fn planned_entry(path: &str, bytes: u64, last_used: u64, prunable: bool) -> Entry {
        Entry {
            path: path.into(),
            bytes,
            last_used,
            prunable,
            purpose: "test",
            source: None,
            recorded: true,
        }
    }

    #[test]
    fn size_policy_is_explicit_and_checked() {
        assert_eq!(parse_size("20GiB").unwrap(), 20 * (1 << 30));
        assert_eq!(parse_size("2gb").unwrap(), 2_000_000_000);
        assert_eq!(parse_size("0").unwrap(), 0);
        for input in ["", "-1", "1.5GiB", "3G", "18446744073709551615TiB"] {
            assert!(parse_size(input).is_err(), "{input}");
        }
        let fixture = Fixture::new();
        assert!(prune(&fixture.0, None, None, false).is_err());
        assert!(prune(&fixture.0, Some(u64::MAX), None, false).is_err());
    }

    #[test]
    fn pruning_combines_expiry_and_oldest_first_capacity() {
        let entries = vec![
            planned_entry("new", 40, 900, true),
            planned_entry("protected", 1000, 1, false),
            planned_entry("old", 30, 100, true),
            planned_entry("middle", 50, 500, true),
            planned_entry("future", 10, 2000, true),
        ];
        assert_eq!(
            plan_prune(&entries, 1000, Some(Duration::from_secs(600)), None),
            [2]
        );
        assert_eq!(plan_prune(&entries, 1000, None, Some(70)), [2, 3]);
        assert_eq!(
            plan_prune(&entries, 1000, Some(Duration::from_secs(200)), Some(40)),
            [2, 3, 0]
        );
        assert_eq!(plan_prune(&entries, 1000, None, Some(0)), [2, 3, 0, 4]);
        assert!(plan_prune(&entries, 1000, None, Some(130)).is_empty());
        assert_eq!(
            plan_prune(&[planned_entry("empty", 0, 100, true)], 1000, None, Some(0)),
            [0]
        );
    }

    #[test]
    fn usage_records_source_and_repeated_use_and_corruption_falls_back() {
        let fixture = Fixture::new();
        let source = fixture.0.join("source");
        fs::create_dir(&source).unwrap();
        let target = fixture.0.join(".scarlet/cache/target/0123456789abcdef");
        record_use(&target, &source);
        record_use(&target, &source);
        let entries = inventory(&fixture.0, true).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].recorded);
        assert_eq!(entries[0].source.as_deref(), Some(source.as_path()));
        assert!(entries[0].last_used >= timestamp(SystemTime::now()) - 2);
        fs::write(target.join(USAGE_FILE), "interrupted JSON").unwrap();
        let entries = inventory(&fixture.0, true).unwrap();
        assert!(!entries[0].recorded);
        assert!(entries[0].last_used >= timestamp(SystemTime::now()) - 2);
    }

    #[test]
    fn newer_files_override_stale_usage_records() {
        let fixture = Fixture::new();
        let artifact = fixture.write(".scarlet/cache/target/0123456789abcdef/debug/artifact");
        let target = artifact.parent().unwrap().parent().unwrap();
        fs::write(
            target.join(USAGE_FILE),
            r#"{"source":"old-source","last_used":1}"#,
        )
        .unwrap();
        let entries = inventory(&fixture.0, true).unwrap();
        assert!(!entries[0].recorded);
        assert!(entries[0].last_used > 1);
    }

    #[test]
    fn prune_dry_run_and_zero_limit_only_remove_known_target_buckets() {
        let fixture = Fixture::new();
        let target = fixture.write(".scarlet/cache/target/0123456789abcdef/debug/output");
        let protected: Vec<_> = [
            ".scarlet/cache/git/source/file",
            ".scarlet/cache/files/download",
            ".scarlet/cache/cargo-home/registry/data",
            ".scarlet/images/rootfs.img",
            ".scarlet/cache/target/debug/custom",
            ".scarlet/cache/target/unknown",
            ".scarlet/cache/custom/data",
            ".scarlet/scarlet-modules/.cargo/config.toml",
        ]
        .iter()
        .map(|path| fixture.write(path))
        .collect();
        let entries = inventory(&fixture.0, false).unwrap();
        assert_eq!(entries.iter().filter(|e| e.prunable).count(), 1);
        assert!(entries.windows(2).all(|w| w[0].bytes >= w[1].bytes));
        prune(&fixture.0, None, Some(0), true).unwrap();
        assert!(target.exists());
        prune(&fixture.0, None, Some(0), false).unwrap();
        assert!(!target.exists());
        for path in protected {
            assert!(path.exists(), "{}", path.display());
        }
    }

    #[test]
    fn legacy_cache_uses_recursive_mtime_for_expiry() {
        let fixture = Fixture::new();
        let output = fixture.write(".scarlet/cache/target/0123456789abcdef/debug/output");
        let target = output.parent().unwrap().parent().unwrap();
        let old = UNIX_EPOCH + Duration::from_secs(1_000_000);
        for path in [&output, output.parent().unwrap(), target] {
            File::open(path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(old))
                .unwrap();
        }
        let entries = inventory(&fixture.0, true).unwrap();
        assert_eq!(entries[0].last_used, timestamp(old));
        prune(&fixture.0, Some(30), None, false).unwrap();
        assert!(!target.exists());
    }

    #[test]
    fn clean_removes_entire_scarlet_but_keeps_other_project_files_and_lock() {
        let fixture = Fixture::new();
        let _lock = ProjectLock::maintenance(&fixture.0).unwrap();
        fixture.write(".scarlet/images/guest.img");
        fixture.write(".scarlet/scarlet-modules/.cargo/config.toml");
        let kept: Vec<_> = [
            "src/main.rs",
            "scarlet.lock",
            "bsp/target/kernel",
            "target/data",
        ]
        .iter()
        .map(|path| fixture.write(path))
        .collect();
        clean(&fixture.0, true).unwrap();
        assert!(fixture.0.join(".scarlet/images/guest.img").exists());
        clean(&fixture.0, false).unwrap();
        assert!(!fixture.0.join(".scarlet").exists());
        assert!(fixture.0.join("scarlet.toml").exists());
        assert!(fixture.0.join(LOCK_FILE).exists());
        assert!(ProjectLock::maintenance(&fixture.0).is_err());
        for path in kept {
            assert!(path.exists());
        }
        clean(&fixture.0, false).unwrap(); // Idempotent, including an empty project.
    }

    #[test]
    fn tracked_cache_files_prevent_any_deletion_even_in_dry_run() {
        let fixture = Fixture::new();
        fixture.git(&["init", "-q"]);
        let kept = fixture.write(".scarlet/cache/target/0123456789abcdef/output");
        let tracked = fixture.write(".scarlet/cache/target/ffffffffffffffff/output");
        fixture.git(&["add", ".scarlet/cache/target/ffffffffffffffff/output"]);
        for dry_run in [true, false] {
            assert!(
                clean(&fixture.0, dry_run)
                    .unwrap_err()
                    .contains("Git-tracked")
            );
            assert!(
                prune(&fixture.0, None, Some(0), dry_run)
                    .unwrap_err()
                    .contains("Git-tracked")
            );
            assert!(kept.exists());
            assert!(tracked.exists());
        }
    }

    #[test]
    fn git_tracking_is_checked_for_projects_below_repo_root() {
        let fixture = Fixture::new();
        fixture.git(&["init", "-q"]);
        let tracked = fixture.write("boards/test/.scarlet/data");
        fixture.git(&["add", "boards/test/.scarlet/data"]);
        assert!(
            clean(&fixture.0.join("boards/test"), false)
                .unwrap_err()
                .contains("Git-tracked")
        );
        assert!(tracked.exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_roots_are_rejected_and_internal_links_never_followed() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = fixture.write("outside/keep");
        symlink(fixture.0.join("outside"), fixture.0.join(".scarlet")).unwrap();
        assert!(clean(&fixture.0, false).unwrap_err().contains("symlink"));
        assert!(prune(&fixture.0, None, Some(0), false).is_err());
        fs::remove_file(fixture.0.join(".scarlet")).unwrap();
        fs::create_dir_all(fixture.0.join(".scarlet/cache")).unwrap();
        symlink(
            fixture.0.join("outside"),
            fixture.0.join(".scarlet/cache/target"),
        )
        .unwrap();
        prune(&fixture.0, None, Some(0), false).unwrap();
        fs::remove_file(fixture.0.join(".scarlet/cache/target")).unwrap();
        fs::create_dir(fixture.0.join(".scarlet/cache/target")).unwrap();
        symlink(
            fixture.0.join("outside"),
            fixture.0.join(".scarlet/cache/target/0123456789abcdef"),
        )
        .unwrap();
        assert!(!inventory(&fixture.0, true).unwrap()[0].prunable);
        prune(&fixture.0, None, Some(0), false).unwrap();
        clean(&fixture.0, false).unwrap();
        assert_eq!(fs::read(outside).unwrap(), vec![42; 8192]);
    }

    #[cfg(unix)]
    #[test]
    fn clean_handles_read_only_directories_without_chmodding_hardlinked_files() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let outside = fixture.write("outside/source");
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o444)).unwrap();
        let cache = fixture.0.join(".scarlet/readonly");
        fs::create_dir_all(&cache).unwrap();
        fs::hard_link(&outside, cache.join("copy")).unwrap();
        fs::set_permissions(cache, fs::Permissions::from_mode(0o555)).unwrap();
        clean(&fixture.0, false).unwrap();
        assert_eq!(
            fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
            0o444
        );
        assert_eq!(fs::read(outside).unwrap(), vec![42; 8192]);
    }

    #[cfg(unix)]
    #[test]
    fn lock_symlink_cannot_redirect_the_coordination_file() {
        let fixture = Fixture::new();
        let outside = fixture.write("outside/file");
        std::os::unix::fs::symlink(&outside, fixture.0.join(LOCK_FILE)).unwrap();
        assert!(ProjectLock::maintenance(&fixture.0).is_err());
        assert_eq!(fs::read(outside).unwrap(), vec![42; 8192]);
    }

    #[test]
    fn lock_process_helper() {
        use std::io::{Read, Write};
        let Some(project) = std::env::var_os("SCARLET_TEST_LOCK_PROJECT") else {
            return;
        };
        let _guard = ProjectLock::activity(Path::new(&project)).unwrap();
        println!("SCARLET_LOCK_HELD");
        std::io::stdout().flush().unwrap();
        let _ = std::io::stdin().read(&mut [0]);
    }

    #[test]
    fn lock_excludes_another_process_and_releases_on_exit() {
        use std::io::{BufRead, BufReader};
        use std::process::Stdio;
        let fixture = Fixture::new();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cache::tests::lock_process_helper",
                "--nocapture",
            ])
            .env("SCARLET_TEST_LOCK_PROJECT", &fixture.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        while !line.contains("SCARLET_LOCK_HELD") {
            line.clear();
            assert_ne!(
                reader.read_line(&mut line).unwrap(),
                0,
                "lock helper exited early"
            );
        }
        assert!(ProjectLock::activity(&fixture.0).is_ok());
        let blocked =
            matches!(ProjectLock::maintenance(&fixture.0), Err(e) if e.contains("in use"));
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        assert!(blocked);
        assert!(ProjectLock::maintenance(&fixture.0).is_ok());
    }
}
