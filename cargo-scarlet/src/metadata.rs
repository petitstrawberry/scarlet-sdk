//! Composition metadata kept outside the guest tree until the filesystem is sealed.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NodeMetadata {
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    /// Full inode mode, including the file-type bits.
    pub(crate) mode: u32,
    pub(crate) mtime_sec: i64,
    pub(crate) mtime_nsec: u32,
    pub(crate) xattrs: BTreeMap<String, Vec<u8>>,
}

impl NodeMetadata {
    pub(crate) fn directory(uid: u32, gid: u32, permissions: u32) -> Self {
        Self {
            uid,
            gid,
            mode: S_IFDIR | (permissions & 0o7777),
            mtime_sec: 0,
            mtime_nsec: 0,
            xattrs: BTreeMap::new(),
        }
    }

    fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }
}

/// Metadata for every explicitly recorded path in one staging tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CompositionMetadata {
    entries: BTreeMap<PathBuf, NodeMetadata>,
}

impl CompositionMetadata {
    #[cfg(test)]
    pub(crate) fn get(&self, path: &Path) -> Option<&NodeMetadata> {
        self.entries.get(path)
    }

    pub(crate) fn invalidate(&mut self, path: &Path) {
        if path.as_os_str().is_empty() {
            return;
        }
        self.entries
            .retain(|candidate, _| candidate != path && !candidate.starts_with(path));
    }

    /// Directory records merge with existing children. Other node types replace
    /// the complete destination subtree so stale capabilities cannot survive.
    pub(crate) fn set_entry(
        &mut self,
        path: PathBuf,
        metadata: NodeMetadata,
    ) -> Result<(), String> {
        ensure_relative(&path)?;
        if !metadata.is_dir() {
            self.invalidate(&path);
        }
        self.entries.insert(path, metadata);
        Ok(())
    }

    pub(crate) fn ensure_parent_directories(&mut self, path: &Path) {
        let mut parents: Vec<_> = path
            .ancestors()
            .skip(1)
            .filter(|parent| !parent.as_os_str().is_empty())
            .collect();
        parents.reverse();
        for parent in parents {
            self.entries
                .entry(parent.to_path_buf())
                .or_insert_with(|| NodeMetadata::directory(0, 0, 0o755));
        }
    }

    /// Capture a local copy layer after its content operation has succeeded.
    pub(crate) fn capture_tree(&mut self, source: &Path, destination: &Path) -> Result<(), String> {
        ensure_relative(destination)?;
        self.capture_tree_inner(source, destination, true)
    }

    pub(crate) fn capture_node(&mut self, source: &Path, destination: &Path) -> Result<(), String> {
        ensure_relative(destination)?;
        self.capture_tree_inner(source, destination, false)
    }

    fn capture_tree_inner(
        &mut self,
        source: &Path,
        destination: &Path,
        skip_root: bool,
    ) -> Result<(), String> {
        let metadata = fs::symlink_metadata(source)
            .map_err(|error| format!("failed to inspect {}: {error}", source.display()))?;
        if !skip_root {
            self.set_entry(destination.to_path_buf(), node_from_host(&metadata))?;
        }
        if !metadata.file_type().is_dir() {
            return Ok(());
        }

        let mut entries: Vec<_> = fs::read_dir(source)
            .map_err(|error| format!("failed to read_dir {}: {error}", source.display()))?
            .collect::<Result<_, _>>()
            .map_err(|error| format!("failed to read entry in {}: {error}", source.display()))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            self.capture_tree_inner(&entry.path(), &destination.join(entry.file_name()), false)?;
        }
        Ok(())
    }

    /// Rebase metadata from a previously composed tree onto a copied image layer.
    pub(crate) fn overlay_tree(
        &mut self,
        source: &CompositionMetadata,
        destination: &Path,
    ) -> Result<(), String> {
        ensure_relative(destination)?;
        for (relative, metadata) in &source.entries {
            let target = if relative.as_os_str().is_empty() {
                destination.to_path_buf()
            } else {
                destination.join(relative)
            };
            self.set_entry(target, metadata.clone())?;
        }
        Ok(())
    }

    pub(crate) fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"scarlet-composition-metadata-v1\0");
        for (path, metadata) in &self.entries {
            hash_field(&mut hasher, path.as_os_str().as_encoded_bytes());
            hasher.update(metadata.uid.to_le_bytes());
            hasher.update(metadata.gid.to_le_bytes());
            hasher.update(metadata.mode.to_le_bytes());
            hasher.update(metadata.mtime_sec.to_le_bytes());
            hasher.update(metadata.mtime_nsec.to_le_bytes());
            hasher.update((metadata.xattrs.len() as u64).to_le_bytes());
            for (name, value) in &metadata.xattrs {
                hash_field(&mut hasher, name.as_bytes());
                hash_field(&mut hasher, value);
            }
        }
        format!("sha256:{:x}", hasher.finalize())
    }

    /// Conservative space for external xattr blocks added after `mke2fs -d`.
    pub(crate) fn ext2_overhead_kib(&self) -> u64 {
        let mut bytes = 0u64;
        for metadata in self
            .entries
            .values()
            .filter(|value| !value.xattrs.is_empty())
        {
            bytes = bytes.saturating_add(4096);
            for (name, value) in &metadata.xattrs {
                bytes = bytes.saturating_add((name.len() + value.len() + 16) as u64);
            }
        }
        bytes.div_ceil(1024)
    }
}

pub(crate) fn apply_ext2_metadata(
    image: &Path,
    metadata: &CompositionMetadata,
) -> Result<(), String> {
    if metadata.entries.is_empty() {
        return Ok(());
    }

    let parent = image
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut sequence = 0u64;
    let work = loop {
        let candidate = parent.join(format!(
            ".cargo-scarlet-metadata-{}-{sequence}",
            std::process::id()
        ));
        match fs::create_dir(&candidate) {
            Ok(()) => break candidate,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                sequence = sequence.saturating_add(1);
            }
            Err(error) => {
                return Err(format!(
                    "failed to create metadata work directory {}: {error}",
                    candidate.display()
                ));
            }
        }
    };
    let result = write_and_apply_metadata_commands(image, metadata, &work);
    let cleanup = fs::remove_dir_all(&work);
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(format!(
            "failed to remove metadata work directory {}: {error}",
            work.display()
        )),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn write_and_apply_metadata_commands(
    image: &Path,
    metadata: &CompositionMetadata,
    work: &Path,
) -> Result<(), String> {
    use std::fmt::Write as _;

    let mut commands = String::new();
    let mut xattr_index = 0u64;
    for (path, node) in &metadata.entries {
        let guest_path = guest_path(path)?;
        let quoted_path = quote_debugfs(&guest_path)?;
        writeln!(commands, "sif {quoted_path} uid {}", node.uid)
            .map_err(|error| error.to_string())?;
        writeln!(commands, "sif {quoted_path} gid {}", node.gid)
            .map_err(|error| error.to_string())?;
        writeln!(commands, "sif {quoted_path} mode {}", node.mode)
            .map_err(|error| error.to_string())?;
        let (mtime_low, mtime_extra) = ext2_mtime_fields(node.mtime_sec, node.mtime_nsec)
            .map_err(|error| format!("{error} at {}", path.display()))?;
        writeln!(commands, "sif {quoted_path} mtime {mtime_low}")
            .map_err(|error| error.to_string())?;
        writeln!(commands, "sif {quoted_path} mtime_extra {mtime_extra}")
            .map_err(|error| error.to_string())?;
        for (name, value) in &node.xattrs {
            let value_path = work.join(format!("xattr-{xattr_index}.bin"));
            xattr_index = xattr_index.saturating_add(1);
            fs::write(&value_path, value).map_err(|error| {
                format!(
                    "failed to stage xattr {name} for {}: {error}",
                    path.display()
                )
            })?;
            writeln!(
                commands,
                "ea_set -f {} {quoted_path} {}",
                quote_debugfs(&value_path.to_string_lossy())?,
                quote_debugfs(name)?
            )
            .map_err(|error| error.to_string())?;
        }
    }
    let commands_path = work.join("commands.debugfs");
    fs::write(&commands_path, commands).map_err(|error| {
        format!(
            "failed to write metadata commands to {}: {error}",
            commands_path.display()
        )
    })?;
    let output = Command::new("debugfs")
        .args([
            "-w",
            "-f",
            commands_path
                .to_str()
                .ok_or("non-UTF-8 debugfs command path")?,
            image.to_str().ok_or("non-UTF-8 ext2 image path")?,
        ])
        .output()
        .map_err(|error| format!("failed to run debugfs: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success()
        || debugfs_stdout_reported_failure(&stdout)
        || debugfs_stderr_reported_failure(&stderr)
    {
        return Err(format!(
            "debugfs metadata application failed: {}",
            stdout
                .lines()
                .chain(stderr.lines())
                .rev()
                .take(8)
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    Ok(())
}

fn ext2_mtime_fields(seconds: i64, nanoseconds: u32) -> Result<(u32, u32), String> {
    // ext4 stores a signed 34-bit second count as a low 32-bit word and two
    // era bits. The era is relative to the sign-extended low word, not a
    // direct `seconds >> 32`. This range starts at 1901-12-13 and ends at
    // 2446-05-10.
    const MIN_MTIME: i64 = -(1i64 << 31);
    const MAX_MTIME: i64 = 3 * (1i64 << 32) + (1i64 << 31) - 1;
    if !(MIN_MTIME..=MAX_MTIME).contains(&seconds) || nanoseconds >= 1_000_000_000 {
        return Err(format!(
            "mtime {seconds}.{nanoseconds:09} is outside ext2's supported range"
        ));
    }
    let low = seconds as u32;
    let low_signed = low as i32 as i64;
    let era = (((seconds - low_signed) >> 32) as u32) & 3;
    Ok((low, era | (nanoseconds << 2)))
}

fn debugfs_stdout_reported_failure(stdout: &str) -> bool {
    stdout.lines().any(|line| {
        let line = line.trim();
        if line.is_empty() {
            return false;
        }
        let Some(command) = line.strip_prefix("debugfs: ") else {
            return true;
        };
        // `debugfs -f` echoes each accepted command. Diagnostics that do not
        // begin with one of those command names are errors, even if debugfs
        // still exits zero.
        !command.starts_with("sif ") && !command.starts_with("ea_set ")
    })
}

fn debugfs_stderr_reported_failure(stderr: &str) -> bool {
    stderr.lines().any(|line| {
        let line = line.trim();
        if line.is_empty() {
            return false;
        }
        let mut fields = line.split_whitespace();
        !(fields.next() == Some("debugfs")
            && fields.next().is_some_and(|version| {
                version.chars().next().is_some_and(|ch| ch.is_ascii_digit())
            }))
    })
}

fn guest_path(path: &Path) -> Result<String, String> {
    ensure_relative(path)?;
    let mut result = String::from("/");
    for component in path.components() {
        if let Component::Normal(component) = component {
            if !result.ends_with('/') {
                result.push('/');
            }
            result.push_str(
                component
                    .to_str()
                    .ok_or_else(|| "non-UTF-8 archive metadata path".to_string())?,
            );
        }
    }
    Ok(result)
}

fn quote_debugfs(value: &str) -> Result<String, String> {
    if value.contains(['\0', '\n', '\r']) {
        return Err("debugfs metadata contains a control character".to_string());
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn ensure_relative(path: &Path) -> Result<(), String> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!(
            "invalid composition metadata path {}",
            path.display()
        ));
    }
    Ok(())
}

fn node_from_host(metadata: &fs::Metadata) -> NodeMetadata {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        NodeMetadata {
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode(),
            mtime_sec: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec().try_into().unwrap_or(0),
            xattrs: BTreeMap::new(),
        }
    }
    #[cfg(not(unix))]
    {
        let mode = if metadata.is_dir() {
            0o040755
        } else if metadata.file_type().is_symlink() {
            0o120777
        } else {
            0o100644
        };
        NodeMetadata {
            uid: 0,
            gid: 0,
            mode,
            mtime_sec: 0,
            mtime_nsec: 0,
            xattrs: BTreeMap::new(),
        }
    }
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debugfs_stdout_distinguishes_echoed_paths_from_diagnostics() {
        assert!(!debugfs_stdout_reported_failure(
            "debugfs: sif \"/var/failed/capability\" uid 0\n\
             debugfs: ea_set -f \"/tmp/errors.bin\" \"/failed\" \"security.capability\"\n"
        ));
        assert!(debugfs_stdout_reported_failure(
            "debugfs: sif \"/missing\" uid 0\ndebugfs: File not found by ext2_lookup\n"
        ));
    }

    #[test]
    fn debugfs_stderr_accepts_only_version_banner() {
        assert!(!debugfs_stderr_reported_failure(
            "\ndebugfs 1.47.4 (6-Mar-2025)\n"
        ));
        assert!(debugfs_stderr_reported_failure(
            "debugfs 1.47.4 (6-Mar-2025)\nOperation not permitted\n"
        ));
    }

    #[test]
    fn ext2_mtime_uses_sign_extended_era_bits() {
        assert_eq!(
            ext2_mtime_fields(2_147_483_648, 250_000_000).unwrap(),
            (0x8000_0000, 0x3b9a_ca01)
        );
        assert_eq!(
            ext2_mtime_fields(-(1i64 << 31), 0).unwrap(),
            (0x8000_0000, 0)
        );
        let maximum = 3 * (1i64 << 32) + (1i64 << 31) - 1;
        assert_eq!(ext2_mtime_fields(maximum, 0).unwrap(), (0x7fff_ffff, 3));
        assert!(ext2_mtime_fields(-(1i64 << 31) - 1, 0).is_err());
        assert!(ext2_mtime_fields(maximum + 1, 0).is_err());
    }
}
