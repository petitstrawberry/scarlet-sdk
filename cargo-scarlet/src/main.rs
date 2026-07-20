use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Parser, Debug)]
#[command(name = "cargo-scarlet")]
#[command(bin_name = "cargo-scarlet")]
#[command(about = "Prototype Scarlet build system generator")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Build {
        #[arg(long)]
        project: Option<PathBuf>,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        release: bool,
        #[arg(long)]
        module: Option<PathBuf>,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        locked: bool,
        #[arg(long)]
        offline: bool,
    },
    Check {
        #[arg(long)]
        project: PathBuf,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        release: bool,
    },
    Clippy {
        #[arg(long)]
        project: PathBuf,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        release: bool,
        #[arg(last = true)]
        extra_args: Vec<String>,
    },
    Run {
        #[arg(long)]
        project: PathBuf,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        release: bool,
        #[arg(long)]
        no_image: bool,
        #[arg(long)]
        locked: bool,
        #[arg(long)]
        offline: bool,
        #[arg(last = true)]
        extra_args: Vec<String>,
    },
    Image {
        #[arg(long)]
        project: PathBuf,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        release: bool,
        #[arg(long)]
        kernel_elf: Option<PathBuf>,
        #[arg(long)]
        no_build: bool,
        #[arg(long)]
        locked: bool,
        #[arg(long)]
        offline: bool,
    },
    New {
        #[arg(long)]
        module: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        kernel_path: Option<PathBuf>,
        #[arg(long)]
        kernel_rev: Option<String>,
        #[arg(long)]
        target: Option<String>,
    },
    Update {
        #[arg(long)]
        project: PathBuf,
        #[arg(long)]
        offline: bool,
    },
}

#[derive(Debug, Deserialize)]
struct ModuleConfig {
    enabled: bool,
    package: Option<String>,
    version: Option<String>,
    path: Option<String>,
    git: Option<String>,
    rev: Option<String>,
    branch: Option<String>,
    tag: Option<String>,
    registry: Option<String>,
    features: Option<Vec<String>>,
    #[serde(rename = "default-features")]
    default_features: Option<bool>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum PackageSource {
    Path(String),
    PathTable {
        path: String,
    },
    Git {
        git: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        rev: Option<String>,
    },
}

impl PackageSource {
    fn to_local_path(&self, base_dir: &Path) -> Option<PathBuf> {
        match self {
            PackageSource::Path(p) => Some(resolve_path(base_dir, p)),
            PackageSource::PathTable { path } => Some(resolve_path(base_dir, path)),
            PackageSource::Git { .. } => None,
        }
    }

    fn is_git(&self) -> bool {
        matches!(self, PackageSource::Git { .. })
    }

    fn git_url(&self) -> Option<&str> {
        match self {
            PackageSource::Git { git, .. } => Some(git),
            _ => None,
        }
    }

    fn git_ref(&self) -> Option<String> {
        match self {
            PackageSource::Git {
                branch: Some(b), ..
            } => Some(format!("refs/heads/{b}")),
            PackageSource::Git { tag: Some(t), .. } => Some(format!("refs/tags/{t}")),
            PackageSource::Git { rev: Some(r), .. } => Some(r.clone()),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ScarletManifest {
    schema_version: u32,
    #[allow(dead_code)]
    project: ManifestProject,
    #[serde(default)]
    bsp: Option<ManifestBsp>,
    #[serde(default)]
    kernel: Option<ManifestKernel>,
    #[serde(default)]
    modules: BTreeMap<String, ModuleConfig>,
    #[serde(default)]
    images: BTreeMap<String, ManifestImageSection>,
    #[serde(default)]
    runner: Option<ManifestRunner>,
}

#[derive(Debug, Deserialize)]
struct ManifestRunner {
    command: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ManifestProject {
    name: String,
}

#[derive(Debug, Deserialize)]
struct ManifestBsp {
    path: String,
    package: String,
    kernel: ManifestBspKernel,
}

#[derive(Debug, Deserialize)]
struct ManifestBspKernel {
    source: PackageSource,
    #[serde(default)]
    features: KernelFeatureConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum KernelFeatureConfig {
    List(Vec<String>),
    States(BTreeMap<String, bool>),
}

impl Default for KernelFeatureConfig {
    fn default() -> Self {
        Self::List(Vec::new())
    }
}

impl KernelFeatureConfig {
    fn enabled(&self) -> Vec<String> {
        match self {
            Self::List(features) => features.clone(),
            Self::States(features) => enabled_feature_names(features),
        }
    }

    fn disabled(&self) -> Vec<String> {
        match self {
            Self::List(_) => Vec::new(),
            Self::States(features) => disabled_feature_names(features),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ManifestKernel {
    package: String,
    source: PackageSource,
    target_json: String,
    #[serde(default)]
    features: BTreeMap<String, bool>,
}

struct BspConfig<'a> {
    root: PathBuf,
    package: &'a str,
    kernel_source: &'a PackageSource,
    kernel_features: Vec<String>,
    disabled_kernel_features: Vec<String>,
    build_target: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ManifestImageSection {
    format: Option<String>,
    output: Option<String>,
    #[serde(default)]
    cmdline: String,
    #[serde(default)]
    dtb: Option<String>,
    #[serde(default)]
    layers: Vec<ManifestLayer>,
    #[serde(default)]
    deps: Vec<String>,
    #[serde(default)]
    partitions: Vec<ManifestGptPartition>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ManifestGptPartition {
    source: String,
    name: String,
    #[serde(rename = "type")]
    type_name: String,
    #[serde(default)]
    flags: u64,
    #[serde(default)]
    alignment_lba: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum ManifestLayer {
    Bundle {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<PackageSource>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subdir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bundle: Option<String>,
    },
    Copy {
        source: String,
        to: String,
        #[serde(default)]
        template: bool,
    },
    Archive {
        url: String,
        sha256: Sha256Spec,
        format: ArchiveFormat,
        #[serde(default)]
        strip_components: usize,
        to: String,
    },
    Cargo {
        source: PackageSource,
        package: Option<String>,
        bin: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        features: Vec<String>,
        #[serde(rename = "default-features")]
        #[serde(skip_serializing_if = "Option::is_none")]
        default_features: Option<bool>,
        #[serde(default, skip_serializing_if = "is_false")]
        replace: bool,
        to: String,
    },
    Script {
        source: String,
        output: Option<String>,
        to: String,
    },
    Image {
        source: String,
        to: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ArchiveFormat {
    Tar,
    TarGz,
    TarZst,
    TarXz,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub(crate) enum Sha256Spec {
    Single(String),
    PerArch(BTreeMap<String, String>),
}

impl Sha256Spec {
    fn resolve(&self, arch: &str) -> Result<String, String> {
        match self {
            Sha256Spec::Single(s) => Ok(s.clone()),
            Sha256Spec::PerArch(map) => map.get(arch).cloned().ok_or_else(|| {
                format!(
                    "archive layer sha256 map is missing entry for arch '{arch}'; have: {}",
                    map.keys().cloned().collect::<Vec<_>>().join(", ")
                )
            }),
        }
    }
}

struct ResolvedSection {
    layers: Vec<ResolvedLayer>,
}

struct ExpandedManifest {
    #[allow(dead_code)]
    project_dir: PathBuf,
    manifest: ScarletManifest,
    sections: BTreeMap<String, ResolvedSection>,
}

#[derive(Serialize)]
struct PluginRequest<'a> {
    project_dir: String,
    section_name: &'a str,
    format: &'a str,
    arch: String,
    kernel_elf: String,
    initramfs: Option<String>,
    output: String,
    section: PluginRequestSection,
}

#[derive(Serialize)]
struct PluginRequestSection {
    cmdline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dtb: Option<String>,
    packages: Vec<PluginRequestPackage>,
}

#[derive(Serialize)]
struct PluginRequestPackage {
    source: String,
    to: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct ImageLock {
    #[serde(default)]
    sections: BTreeMap<String, SectionLock>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SectionLock {
    hash: String,
    #[serde(default)]
    layers: Vec<LayerLock>,
    #[serde(default, skip_serializing)]
    files: Vec<FileLock>,
    #[serde(default, skip_serializing)]
    packages: Vec<PackageLock>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct FileLock {
    source: String,
    #[serde(default)]
    to: String,
    #[serde(default, skip_serializing_if = "is_false")]
    template: bool,
    hash: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PackageLock {
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<LockPackageSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_rev: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bin: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    features: Vec<String>,
    #[serde(rename = "default-features")]
    #[serde(skip_serializing_if = "Option::is_none")]
    default_features: Option<bool>,
    #[serde(default)]
    to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    hash: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum LayerLock {
    Copy {
        source: String,
        #[serde(default)]
        to: String,
        #[serde(default, skip_serializing_if = "is_false")]
        template: bool,
        hash: String,
    },
    Archive {
        source: LockPackageSource,
        #[serde(default)]
        to: String,
        format: ArchiveFormat,
        #[serde(default)]
        strip_components: usize,
        hash: String,
    },
    Cargo {
        #[serde(skip_serializing_if = "Option::is_none")]
        source: Option<LockPackageSource>,
        #[serde(skip_serializing_if = "Option::is_none")]
        git: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        git_ref: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        resolved_rev: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        package: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        bin: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        features: Vec<String>,
        #[serde(rename = "default-features")]
        #[serde(skip_serializing_if = "Option::is_none")]
        default_features: Option<bool>,
        #[serde(default)]
        to: String,
        hash: String,
    },
    Script {
        #[serde(skip_serializing_if = "Option::is_none")]
        source: Option<LockPackageSource>,
        #[serde(default)]
        to: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        hash: String,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum LockPackageSource {
    Structured(StructuredPackageSource),
    LegacyPath(String),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum StructuredPackageSource {
    Path { path: String },
    Git { url: String, rev: String },
    Archive { url: String, sha256: String },
}

impl LockPackageSource {
    fn path(path: String) -> Self {
        Self::Structured(StructuredPackageSource::Path { path })
    }

    fn git(url: String, rev: String) -> Self {
        Self::Structured(StructuredPackageSource::Git { url, rev })
    }

    fn archive(url: String, sha256: String) -> Self {
        Self::Structured(StructuredPackageSource::Archive { url, sha256 })
    }
}

impl SectionLock {
    fn package_locks(&self) -> impl Iterator<Item = PackageLock> + '_ {
        self.packages
            .iter()
            .cloned()
            .chain(self.layers.iter().filter_map(|layer| match layer {
                LayerLock::Cargo {
                    source,
                    git,
                    git_ref,
                    resolved_rev,
                    package,
                    bin,
                    features,
                    default_features,
                    to,
                    hash,
                } => Some(PackageLock {
                    kind: "cargo".to_string(),
                    source: source.clone(),
                    git: git.clone(),
                    git_ref: git_ref.clone(),
                    resolved_rev: resolved_rev.clone(),
                    package: package.clone(),
                    bin: bin.clone(),
                    features: features.clone(),
                    default_features: *default_features,
                    to: to.clone(),
                    output: None,
                    hash: hash.clone(),
                }),
                LayerLock::Script {
                    source,
                    to,
                    output,
                    hash,
                } => Some(PackageLock {
                    kind: "script".to_string(),
                    source: source.clone(),
                    git: None,
                    git_ref: None,
                    resolved_rev: None,
                    package: None,
                    bin: None,
                    features: Vec::new(),
                    default_features: None,
                    to: to.clone(),
                    output: output.clone(),
                    hash: hash.clone(),
                }),
                LayerLock::Copy { .. } | LayerLock::Archive { .. } => None,
            }))
    }

    fn copy_locks(&self) -> impl Iterator<Item = FileLock> + '_ {
        self.files
            .iter()
            .cloned()
            .chain(self.layers.iter().filter_map(|layer| match layer {
                LayerLock::Copy {
                    source,
                    to,
                    template,
                    hash,
                } => Some(FileLock {
                    source: source.clone(),
                    to: to.clone(),
                    template: *template,
                    hash: hash.clone(),
                }),
                _ => None,
            }))
    }
}

fn package_lock_to_layer(lock: PackageLock) -> LayerLock {
    match lock.kind.as_str() {
        "script" => LayerLock::Script {
            source: lock.source,
            to: lock.to,
            output: lock.output,
            hash: lock.hash,
        },
        _ => LayerLock::Cargo {
            source: lock.source,
            git: lock.git,
            git_ref: lock.git_ref,
            resolved_rev: lock.resolved_rev,
            package: lock.package,
            bin: lock.bin,
            features: lock.features,
            default_features: lock.default_features,
            to: lock.to,
            hash: lock.hash,
        },
    }
}

fn package_git_url(pkg: &ResolvedPackage) -> Option<String> {
    pkg.source
        .as_ref()
        .and_then(|source| source.git_url())
        .map(str::to_string)
}

fn package_git_ref(pkg: &ResolvedPackage) -> Option<String> {
    pkg.source.as_ref().and_then(PackageSource::git_ref)
}

fn package_output_lock_path(
    project: &Path,
    pkg: &ResolvedPackage,
) -> Result<Option<String>, String> {
    pkg.output
        .as_ref()
        .map(|output| pathdiff(output, project).map(|path| path.to_string_lossy().to_string()))
        .transpose()
}

fn package_lock_matches_input(
    project: &Path,
    lock: &PackageLock,
    pkg: &ResolvedPackage,
) -> Result<bool, String> {
    if lock.kind != pkg.kind.as_deref().unwrap_or("") {
        return Ok(false);
    }
    if lock.package != pkg.package_name {
        return Ok(false);
    }
    if lock.bin != pkg.bin {
        return Ok(false);
    }
    if lock.features != pkg.features {
        return Ok(false);
    }
    if lock.default_features != pkg.default_features {
        return Ok(false);
    }
    if lock.to != pkg.to {
        return Ok(false);
    }
    if lock.output != package_output_lock_path(project, pkg)? {
        return Ok(false);
    }

    if let Some(git) = package_git_url(pkg) {
        return Ok(
            lock.git.as_deref() == Some(git.as_str()) && lock.git_ref == package_git_ref(pkg)
        );
    }

    Ok(lock.source == package_lock_source(project, pkg)?)
}

fn copy_lock_matches_input(lock: &FileLock, file: &ResolvedFile) -> bool {
    match &file.source {
        FileSource::Url(url) => {
            lock.source == *url && lock.to == file.to && lock.template == file.template
        }
        FileSource::Local(_) => false,
    }
}

fn archive_lock_matches_input(lock: &LayerLock, archive: &ResolvedArchive) -> bool {
    let LayerLock::Archive {
        source,
        to,
        format,
        strip_components,
        hash,
    } = lock
    else {
        return false;
    };
    let LockPackageSource::Structured(StructuredPackageSource::Archive { url, sha256 }) = source
    else {
        return false;
    };

    url == &archive.url
        && to == &archive.to
        && format == &archive.format
        && strip_components == &archive.strip_components
        && normalize_sha256(sha256).ok().as_deref() == Some(archive.sha256.as_str())
        && normalize_sha256(hash).ok().as_deref() == Some(archive.sha256.as_str())
}

fn archive_lock_matches_destination(lock: &LayerLock, to: &str) -> bool {
    matches!(lock, LayerLock::Archive { to: lock_to, .. } if lock_to == to)
}

fn validate_locked_archive_layers(
    expanded: &ExpandedManifest,
    existing_lock: &ImageLock,
) -> Result<(), String> {
    for (section_name, section) in &expanded.sections {
        let section_lock = existing_lock.sections.get(section_name);
        for archive in section.layers.iter().filter_map(|layer| match layer {
            ResolvedLayer::Archive(archive) => Some(archive),
            _ => None,
        }) {
            let locks = section_lock
                .map(|lock| lock.layers.as_slice())
                .unwrap_or(&[]);
            let has_destination = locks
                .iter()
                .any(|lock| archive_lock_matches_destination(lock, &archive.to));
            let has_identity = locks.iter().any(|lock| {
                matches!(
                    lock,
                    LayerLock::Archive {
                        source: LockPackageSource::Structured(StructuredPackageSource::Archive { url, .. }),
                        to,
                        ..
                    } if url == &archive.url && to == &archive.to
                )
            });

            if !has_identity {
                let state = if has_destination {
                    "differs"
                } else {
                    "is missing"
                };
                return Err(format!(
                    "--locked: archive layer {} in section {} {state} from scarlet.lock; run `cargo scarlet update`",
                    archive.url, section_name
                ));
            }
            if !locks
                .iter()
                .any(|lock| archive_lock_matches_input(lock, archive))
            {
                return Err(format!(
                    "--locked: archive layer {} in section {} differs from scarlet.lock; run `cargo scarlet update`",
                    archive.url, section_name
                ));
            }
        }
    }
    Ok(())
}

fn package_input_lock(
    project: &Path,
    pkg: &ResolvedPackage,
    hash: String,
) -> Result<PackageLock, String> {
    let git = package_git_url(pkg);
    let source = match (&git, &pkg.resolved_rev) {
        (Some(url), Some(rev)) => Some(LockPackageSource::git(url.clone(), rev.clone())),
        _ => package_lock_source(project, pkg)?,
    };
    Ok(PackageLock {
        kind: pkg.kind.clone().unwrap_or_default(),
        source,
        git,
        git_ref: package_git_ref(pkg),
        resolved_rev: pkg.resolved_rev.clone(),
        package: pkg.package_name.clone(),
        bin: pkg.bin.clone(),
        features: pkg.features.clone(),
        default_features: pkg.default_features,
        to: pkg.to.clone(),
        output: package_output_lock_path(project, pkg)?,
        hash,
    })
}

fn find_package_lock_for_input(
    project: &Path,
    section_lock: &SectionLock,
    pkg: &ResolvedPackage,
) -> Result<Option<PackageLock>, String> {
    if let Some(lock) = section_lock
        .package_locks()
        .find(|lock| package_lock_matches_input(project, lock, pkg).unwrap_or(false))
    {
        return Ok(Some(lock));
    }

    let source = package_lock_source(project, pkg)?;
    Ok(section_lock.package_locks().find(|lock| {
        lock.kind == pkg.kind.as_deref().unwrap_or("")
            && lock.source == source
            && lock.bin == pkg.bin
            && lock.features == pkg.features
            && lock.default_features == pkg.default_features
            && lock.git == package_git_url(pkg)
    }))
}

struct ResolvedPackage {
    kind: Option<String>,
    source: Option<PackageSource>,
    local_source: Option<PathBuf>,
    resolved_rev: Option<String>,
    package_name: Option<String>,
    bin: Option<String>,
    features: Vec<String>,
    default_features: Option<bool>,
    from: Option<PathBuf>,
    to: String,
    output: Option<PathBuf>,
}

struct PackageLayerSpec {
    kind: String,
    source: Option<PackageSource>,
    package: Option<String>,
    bin: Option<String>,
    features: Vec<String>,
    default_features: Option<bool>,
    from: Option<String>,
    to: String,
    output: Option<String>,
}

#[allow(clippy::large_enum_variant)]
enum ResolvedLayer {
    Copy(ResolvedFile),
    Archive(ResolvedArchive),
    Package(ResolvedPackage),
    Image { source: String, to: String },
}

struct ResolvedArchive {
    url: String,
    sha256: String,
    format: ArchiveFormat,
    strip_components: usize,
    to: String,
}

impl ResolvedSection {
    fn packages_mut(&mut self) -> impl Iterator<Item = &mut ResolvedPackage> {
        self.layers.iter_mut().filter_map(|layer| match layer {
            ResolvedLayer::Package(pkg) => Some(pkg),
            _ => None,
        })
    }
}

fn load_manifest(project_dir: &Path) -> Result<ScarletManifest, String> {
    let manifest_path = project_dir.join("scarlet.toml");
    let text = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("failed to read {}: {e}", manifest_path.display()))?;
    let mut root: toml::Value = toml::from_str(&text)
        .map_err(|e| format!("failed to parse {}: {e}", manifest_path.display()))?;

    let local_path = project_dir.join("scarlet.local.toml");
    if local_path.exists() {
        let local_text = fs::read_to_string(&local_path)
            .map_err(|e| format!("failed to read {}: {e}", local_path.display()))?;
        let local_value: toml::Value = toml::from_str(&local_text)
            .map_err(|e| format!("failed to parse {}: {e}", local_path.display()))?;
        merge_toml_into(&mut root, local_value);
        eprintln!("cargo-scarlet: applied overrides from scarlet.local.toml");
    }

    let merged_text = toml::to_string(&root)
        .map_err(|e| format!("failed to re-serialize merged manifest: {e}"))?;
    let manifest: ScarletManifest =
        toml::from_str(&merged_text).map_err(|e| format!("failed to deserialize manifest: {e}"))?;

    if manifest.schema_version != 2 {
        return Err(format!(
            "unsupported schema_version {} (expected 2)",
            manifest.schema_version
        ));
    }

    Ok(manifest)
}

fn merge_toml_into(parent: &mut toml::Value, child: toml::Value) {
    let toml::Value::Table(parent_table) = parent else {
        return;
    };
    let toml::Value::Table(child_table) = child else {
        return;
    };
    for (key, child_val) in child_table {
        match parent_table.get_mut(&key) {
            Some(toml::Value::Array(parent_arr)) => {
                if let toml::Value::Array(child_arr) = child_val {
                    parent_arr.extend(child_arr);
                }
            }
            Some(parent_existing) => {
                let child_tables = matches!(parent_existing, toml::Value::Table(_))
                    && matches!(&child_val, toml::Value::Table(_));
                if child_tables {
                    let mut taken = parent_existing.clone();
                    merge_toml_into(&mut taken, child_val);
                    parent_table.insert(key, taken);
                }
            }
            _ => {
                parent_table.insert(key, child_val);
            }
        }
    }
}

fn resolve_package(
    pkg: &PackageLayerSpec,
    base_dir: &Path,
    target_triple: &str,
    images: &BTreeMap<String, ManifestImageSection>,
) -> ResolvedPackage {
    let arch = target_triple.split('-').next().unwrap_or("unknown");
    let source = pkg.source.as_ref().map(|s| match s {
        PackageSource::Path(p) => {
            let expanded = expand_templates(p, target_triple, arch);
            PackageSource::Path(expanded)
        }
        PackageSource::PathTable { path } => {
            let expanded = expand_templates(path, target_triple, arch);
            PackageSource::PathTable { path: expanded }
        }
        PackageSource::Git {
            git,
            branch,
            tag,
            rev,
        } => PackageSource::Git {
            git: git.clone(),
            branch: branch.clone(),
            tag: tag.clone(),
            rev: rev.clone(),
        },
    });
    let local_source = source.as_ref().and_then(|s| s.to_local_path(base_dir));
    ResolvedPackage {
        kind: Some(pkg.kind.clone()),
        source,
        local_source,
        resolved_rev: None,
        package_name: pkg.package.clone(),
        bin: pkg.bin.clone(),
        features: pkg.features.clone(),
        default_features: pkg.default_features,
        from: pkg.from.as_ref().and_then(|s| {
            if images.contains_key(s.as_str()) {
                None
            } else {
                Some(resolve_path(
                    base_dir,
                    &expand_templates(s, target_triple, arch),
                ))
            }
        }),
        to: expand_templates(&pkg.to, target_triple, arch),
        output: pkg.output.as_ref().map(|o| resolve_path(base_dir, o)),
    }
}

fn resolve_path(base: &Path, relative: &str) -> PathBuf {
    let path = Path::new(relative);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn manifest_bsp_config<'a>(
    manifest: &'a ScarletManifest,
    project_dir: &Path,
) -> Result<BspConfig<'a>, String> {
    if let Some(bsp) = &manifest.bsp {
        let root = resolve_path(project_dir, &bsp.path);
        let build_target = read_bsp_build_target(&root)?;
        return Ok(BspConfig {
            root,
            package: &bsp.package,
            kernel_source: &bsp.kernel.source,
            kernel_features: bsp.kernel.features.enabled(),
            disabled_kernel_features: bsp.kernel.features.disabled(),
            build_target,
        });
    }

    let kernel = manifest
        .kernel
        .as_ref()
        .ok_or("scarlet.toml must contain [bsp] or legacy [kernel]")?;
    Ok(BspConfig {
        root: project_dir.to_path_buf(),
        package: &kernel.package,
        kernel_source: &kernel.source,
        kernel_features: enabled_feature_names(&kernel.features),
        disabled_kernel_features: disabled_feature_names(&kernel.features),
        build_target: kernel.target_json.clone(),
    })
}

fn read_bsp_build_target(bsp_root: &Path) -> Result<String, String> {
    let config_path = bsp_root.join(".cargo/config.toml");
    let config = fs::read_to_string(&config_path)
        .map_err(|e| format!("failed to read {}: {e}", config_path.display()))?;
    let value: toml::Value = toml::from_str(&config)
        .map_err(|e| format!("failed to parse {}: {e}", config_path.display()))?;
    value
        .get("build")
        .and_then(|build| build.get("target"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("{} must define [build].target", config_path.display()))
}

fn enabled_feature_names(features: &BTreeMap<String, bool>) -> Vec<String> {
    features
        .iter()
        .filter(|(_, enabled)| **enabled)
        .map(|(feature, _)| feature.clone())
        .collect()
}

fn disabled_feature_names(features: &BTreeMap<String, bool>) -> Vec<String> {
    features
        .iter()
        .filter(|(_, enabled)| !**enabled)
        .map(|(feature, _)| feature.clone())
        .collect()
}

fn target_triple_from_build_target(build_target: &str) -> Result<String, String> {
    Path::new(build_target)
        .file_stem()
        .ok_or_else(|| format!("target path has no file stem: {build_target}"))
        .map(|stem| stem.to_string_lossy().to_string())
}

fn build_target_arg(bsp_root: &Path, build_target: &str) -> String {
    let path = Path::new(build_target);
    if path.is_absolute() {
        build_target.to_string()
    } else {
        bsp_root.join(path).display().to_string()
    }
}

fn git_resolve_rev(url: &str, refspec: &str) -> Result<String, String> {
    let output = Command::new("git")
        .arg("ls-remote")
        .arg(url)
        .arg(refspec)
        .output()
        .map_err(|e| format!("failed to run git ls-remote: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git ls-remote failed for {url}: {stderr}"));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .next()
        .ok_or_else(|| format!("git ls-remote returned no output for {url} {refspec}"))?;
    let rev = line
        .split_whitespace()
        .next()
        .ok_or_else(|| format!("git ls-remote unexpected output: {line}"))?;
    if rev.len() < 40 {
        return Err(format!("git ls-remote unexpected rev: {rev}"));
    }
    Ok(rev[..40].to_string())
}

fn git_cache_dir_for_url(url: &str, cache_base: &Path) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    let hash = hex::encode(hasher.finalize());
    cache_base.join(&hash[..16])
}

fn project_cargo_target_dir(project: &Path) -> PathBuf {
    project.join(".scarlet/cache/target")
}

fn git_ensure_checkout(
    url: &str,
    rev: &str,
    cache_base: &Path,
    offline: bool,
) -> Result<PathBuf, String> {
    let dir = git_cache_dir_for_url(url, cache_base);
    if dir.join(".git").exists() {
        let head_rev = git_current_rev(&dir)?;
        if head_rev == rev {
            return Ok(dir);
        }
        if offline {
            let output = Command::new("git")
                .args(["rev-parse", "--verify", &format!("{rev}^{{commit}}")])
                .current_dir(&dir)
                .output()
                .map_err(|error| format!("failed to inspect cached git source: {error}"))?;
            if !output.status.success() {
                return Err(format!(
                    "--offline: cached git source {url} does not contain revision {rev}"
                ));
            }
        } else {
            let status = Command::new("git")
                .arg("fetch")
                .arg("origin")
                .current_dir(&dir)
                .status()
                .map_err(|e| format!("git fetch failed: {e}"))?;
            if !status.success() {
                return Err(format!("git fetch failed in {}", dir.display()));
            }
        }
    } else if offline {
        return Err(format!(
            "--offline: git source {url} is not present in .scarlet/cache/git"
        ));
    } else {
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("failed to create cache dir: {e}"))?;
        }
        let status = Command::new("git")
            .arg("clone")
            .arg(url)
            .arg(&dir)
            .status()
            .map_err(|e| format!("git clone failed: {e}"))?;
        if !status.success() {
            return Err(format!("git clone failed for {url}"));
        }
    }
    let status = Command::new("git")
        .arg("checkout")
        .arg(rev)
        .current_dir(&dir)
        .status()
        .map_err(|e| format!("git checkout failed: {e}"))?;
    if !status.success() {
        return Err(format!("git checkout {rev} failed in {}", dir.display()));
    }
    Ok(dir)
}

fn git_resolve_cached_rev(url: &str, refspec: &str, cache_base: &Path) -> Result<String, String> {
    let dir = git_cache_dir_for_url(url, cache_base);
    if !dir.join(".git").exists() {
        return Err(format!(
            "--offline: git source {url} is not present in .scarlet/cache/git"
        ));
    }
    let output = Command::new("git")
        .args(["rev-parse", "--verify", &format!("{refspec}^{{commit}}")])
        .current_dir(&dir)
        .output()
        .map_err(|error| format!("failed to inspect cached git source: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "--offline: cached git source {url} does not contain reference {refspec}"
        ));
    }
    let rev = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !is_full_git_commit_id(&rev) {
        return Err(format!(
            "--offline: cached git source {url} resolved invalid revision {rev}"
        ));
    }
    Ok(rev)
}

fn git_current_rev(dir: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .map_err(|e| format!("failed to run git rev-parse: {e}"))?;
    if !output.status.success() {
        return Err(format!("git rev-parse failed in {}", dir.display()));
    }
    let rev = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if rev.len() < 40 {
        return Err(format!("git rev-parse unexpected output: {rev}"));
    }
    Ok(rev[..40].to_string())
}

fn resolve_git_source(
    source: &PackageSource,
    cache_dir: &Path,
    locked_rev: Option<String>,
    offline: bool,
) -> Result<(PathBuf, String), String> {
    let PackageSource::Git {
        git,
        branch,
        tag,
        rev,
    } = source
    else {
        return Err("source is not a git source".to_string());
    };

    let resolved_rev = if let Some(rev) = locked_rev {
        eprintln!("cargo-scarlet: using locked {} -> {rev}", git);
        rev
    } else {
        let refspec = rev
            .as_deref()
            .or(tag.as_deref())
            .or(branch.as_deref())
            .unwrap_or("HEAD");
        let resolved_rev = if is_full_git_commit_id(refspec) {
            refspec.to_string()
        } else if offline {
            git_resolve_cached_rev(git, refspec, cache_dir)?
        } else {
            git_resolve_rev(git, refspec)?
        };
        eprintln!(
            "cargo-scarlet: resolved {} {} -> {resolved_rev}",
            git, refspec
        );
        resolved_rev
    };
    let checkout_dir = git_ensure_checkout(git, &resolved_rev, cache_dir, offline)?;
    Ok((checkout_dir, resolved_rev))
}

fn is_full_git_commit_id(revision: &str) -> bool {
    revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn resolve_git_sources(
    expanded: &mut ExpandedManifest,
    project: &Path,
    existing_lock: &ImageLock,
    offline: bool,
) -> Result<(), String> {
    let cache_dir = project.join(".scarlet/cache/git");
    for (section_name, section) in expanded.sections.iter_mut() {
        for pkg in section.packages_mut() {
            let Some(source @ PackageSource::Git { .. }) = pkg.source.as_ref() else {
                continue;
            };
            if pkg.local_source.is_some() {
                continue;
            }

            let locked_rev = existing_lock
                .sections
                .get(section_name)
                .and_then(|s| {
                    s.package_locks()
                        .find(|p| package_lock_matches_input(project, p, pkg).unwrap_or(false))
                })
                .and_then(|p| p.resolved_rev.clone());
            let (checkout_dir, resolved_rev) =
                resolve_git_source(source, &cache_dir, locked_rev, offline)?;
            pkg.local_source = Some(checkout_dir);
            pkg.resolved_rev = Some(resolved_rev);
        }
    }
    Ok(())
}

struct TemplateContext {
    arch: String,
    target_triple: String,
    project: String,
}

impl TemplateContext {
    fn arch(&self) -> &str {
        &self.arch
    }

    fn expand(&self, s: &str) -> String {
        s.replace("{target_triple}", &self.target_triple)
            .replace("{arch}", &self.arch)
            .replace("{project}", &self.project)
    }
}

fn expand_templates(s: &str, target_triple: &str, arch: &str) -> String {
    s.replace("{target_triple}", target_triple)
        .replace("{arch}", arch)
}

fn userspace_target_triple(kernel_triple: &str) -> String {
    let arch = kernel_triple.split('-').next().unwrap_or("unknown");
    match arch {
        "aarch64" => "aarch64-unknown-scarlet".to_string(),
        v if v.starts_with("riscv64") => "riscv64gc-unknown-scarlet".to_string(),
        _ => kernel_triple.to_string(),
    }
}

#[derive(Debug)]
enum FileSource {
    Local(PathBuf),
    Url(String),
}

struct ResolvedFile {
    source: FileSource,
    to: String,
    template: bool,
}

#[derive(Debug, Default, Deserialize)]
struct BundleManifest {
    #[serde(default)]
    layers: Vec<ManifestLayer>,
}

fn resolve_section(
    section: &ManifestImageSection,
    base_dir: &Path,
    ctx: &TemplateContext,
    images: &BTreeMap<String, ManifestImageSection>,
    offline: bool,
) -> Result<ResolvedSection, String> {
    let layers = resolve_layers_with_offline(&section.layers, base_dir, ctx, images, offline)?;
    Ok(ResolvedSection { layers })
}

#[cfg(test)]
fn resolve_layers(
    layers: &[ManifestLayer],
    base_dir: &Path,
    ctx: &TemplateContext,
    images: &BTreeMap<String, ManifestImageSection>,
) -> Result<Vec<ResolvedLayer>, String> {
    resolve_layers_with_offline(layers, base_dir, ctx, images, false)
}

fn resolve_layers_with_offline(
    layers: &[ManifestLayer],
    base_dir: &Path,
    ctx: &TemplateContext,
    images: &BTreeMap<String, ManifestImageSection>,
    offline: bool,
) -> Result<Vec<ResolvedLayer>, String> {
    let mut resolved = Vec::new();
    let git_cache_dir = base_dir.join(".scarlet/cache/git");
    resolve_layers_into(
        &mut resolved,
        layers,
        base_dir,
        ctx,
        images,
        &git_cache_dir,
        offline,
    )?;
    Ok(resolved)
}

fn resolve_bundle_path(
    path: Option<&str>,
    source: Option<&PackageSource>,
    subdir: Option<&str>,
    bundle: Option<&str>,
    base_dir: &Path,
    git_cache_dir: &Path,
    offline: bool,
) -> Result<PathBuf, String> {
    match source {
        Some(source @ PackageSource::Git { .. }) => {
            let (checkout_dir, _) = resolve_git_source(source, git_cache_dir, None, offline)
                .map_err(|error| format!("failed to resolve git bundle source: {error}"))?;
            let subdir = Path::new(subdir.unwrap_or(""));
            let bundle = Path::new(bundle.unwrap_or("bundle.toml"));
            if subdir.is_absolute() || bundle.is_absolute() {
                return Err(
                    "git bundle source subdir and bundle must be relative to the checkout root"
                        .to_string(),
                );
            }
            Ok(checkout_dir.join(subdir).join(bundle))
        }
        Some(_) => Err(
            "bundle layer source must be a git source; use path for a local bundle file"
                .to_string(),
        ),
        None => path
            .map(|path| resolve_path(base_dir, path))
            .ok_or_else(|| "bundle layer requires path or a git source".to_string()),
    }
}

fn resolve_layers_into(
    resolved: &mut Vec<ResolvedLayer>,
    layers: &[ManifestLayer],
    base_dir: &Path,
    ctx: &TemplateContext,
    images: &BTreeMap<String, ManifestImageSection>,
    git_cache_dir: &Path,
    offline: bool,
) -> Result<(), String> {
    for layer in layers {
        match layer {
            ManifestLayer::Bundle {
                path,
                source,
                subdir,
                bundle,
            } => {
                let path = path.as_deref().map(|path| ctx.expand(path));
                let subdir = subdir.as_deref().map(|subdir| ctx.expand(subdir));
                let bundle = bundle.as_deref().map(|bundle| ctx.expand(bundle));
                let bundle_path = resolve_bundle_path(
                    path.as_deref(),
                    source.as_ref(),
                    subdir.as_deref(),
                    bundle.as_deref(),
                    base_dir,
                    git_cache_dir,
                    offline,
                )?;
                let bundle_dir = bundle_path.parent().unwrap_or(Path::new("."));
                let text = fs::read_to_string(&bundle_path)
                    .map_err(|e| format!("failed to read bundle {}: {e}", bundle_path.display()))?;
                let bundle: BundleManifest = toml::from_str(&text).map_err(|e| {
                    format!("failed to parse bundle {}: {e}", bundle_path.display())
                })?;
                resolve_layers_into(
                    resolved,
                    &bundle.layers,
                    bundle_dir,
                    ctx,
                    images,
                    git_cache_dir,
                    offline,
                )?;
            }
            ManifestLayer::Copy {
                source,
                to,
                template,
            } => {
                let expanded_source = ctx.expand(source);
                let source = if expanded_source.starts_with("https://")
                    || expanded_source.starts_with("http://")
                {
                    FileSource::Url(expanded_source)
                } else {
                    FileSource::Local(resolve_path(base_dir, &expanded_source))
                };
                resolved.push(ResolvedLayer::Copy(ResolvedFile {
                    source,
                    to: ctx.expand(to),
                    template: *template,
                }));
            }
            ManifestLayer::Archive {
                url,
                sha256,
                format,
                strip_components,
                to,
            } => {
                let url = ctx.expand(url);
                let sha256 = sha256
                    .resolve(ctx.arch())
                    .and_then(|sha256| normalize_sha256(&sha256))
                    .map_err(|error| format!("archive layer {url}: {error}"))?;
                resolved.push(ResolvedLayer::Archive(ResolvedArchive {
                    url,
                    sha256,
                    format: format.clone(),
                    strip_components: *strip_components,
                    to: ctx.expand(to),
                }));
            }
            ManifestLayer::Cargo {
                source,
                package,
                bin,
                features,
                default_features,
                replace,
                to,
            } => {
                let pkg = PackageLayerSpec {
                    kind: "cargo".to_string(),
                    source: Some(source.clone()),
                    package: package.clone(),
                    bin: bin.clone(),
                    features: features.clone(),
                    default_features: *default_features,
                    from: None,
                    to: to.clone(),
                    output: None,
                };
                let resolved_pkg = resolve_package(&pkg, base_dir, &ctx.target_triple, images);
                if *replace {
                    resolved.retain(|layer| {
                        !matches!(
                            layer,
                            ResolvedLayer::Package(existing)
                                if existing.kind.as_deref() == Some("cargo")
                                    && existing.to == resolved_pkg.to
                        )
                    });
                }
                resolved.push(ResolvedLayer::Package(resolved_pkg));
            }
            ManifestLayer::Script { source, output, to } => {
                let pkg = PackageLayerSpec {
                    kind: "script".to_string(),
                    source: Some(PackageSource::Path(source.clone())),
                    package: None,
                    bin: None,
                    features: Vec::new(),
                    default_features: None,
                    from: None,
                    to: to.clone(),
                    output: output.clone(),
                };
                resolved.push(ResolvedLayer::Package(resolve_package(
                    &pkg,
                    base_dir,
                    &ctx.target_triple,
                    images,
                )));
            }
            ManifestLayer::Image { source, to } => {
                if !images.contains_key(source) {
                    return Err(format!("image layer references unknown image '{}'", source));
                }
                resolved.push(ResolvedLayer::Image {
                    source: source.clone(),
                    to: ctx.expand(to),
                });
            }
        }
    }
    Ok(())
}

fn expand_manifest_with_offline(
    project_dir: &Path,
    offline: bool,
) -> Result<ExpandedManifest, String> {
    let manifest = load_manifest(project_dir)?;
    let bsp = manifest_bsp_config(&manifest, project_dir)?;
    let target_triple = target_triple_from_build_target(&bsp.build_target)?;
    let raw_arch = target_triple.split('-').next().unwrap_or("unknown");
    let arch = match raw_arch {
        "riscv64gc" => "riscv64".to_string(),
        other => other.to_string(),
    };
    let project = manifest.project.name.clone();

    let ctx = TemplateContext {
        arch,
        target_triple,
        project,
    };

    let mut sections = BTreeMap::new();
    let images_ref = &manifest.images;
    for (name, section) in images_ref {
        sections.insert(
            name.clone(),
            resolve_section(section, project_dir, &ctx, images_ref, offline)?,
        );
    }

    Ok(ExpandedManifest {
        project_dir: project_dir.to_path_buf(),
        manifest,
        sections,
    })
}

fn generate_from_manifest(project_dir: &Path) -> Result<ExpandedManifest, String> {
    generate_from_manifest_with_offline(project_dir, false)
}

fn generate_from_manifest_with_offline(
    project_dir: &Path,
    offline: bool,
) -> Result<ExpandedManifest, String> {
    let expanded = expand_manifest_with_offline(project_dir, offline)?;

    let generated_root = project_dir.join(".scarlet/scarlet-modules");
    let generated_src = generated_root.join("src");
    let generated_cargo = generated_root.join(".cargo");
    fs::create_dir_all(&generated_src)
        .map_err(|e| format!("failed to create {}: {e}", generated_src.display()))?;
    fs::create_dir_all(&generated_cargo)
        .map_err(|e| format!("failed to create {}: {e}", generated_cargo.display()))?;

    let cargo_toml = render_manifest_cargo_toml(&expanded.manifest, project_dir)?;
    let lib_rs = render_manifest_lib_rs(&expanded.manifest);

    write_if_changed(&generated_root.join("Cargo.toml"), &cargo_toml)?;
    write_if_changed(&generated_src.join("lib.rs"), &lib_rs)?;

    let cargo_config_path = generated_cargo.join("config.toml");
    if !cargo_config_path.exists() {
        let cargo_config = render_cargo_config();
        fs::write(&cargo_config_path, cargo_config)
            .map_err(|e| format!("failed to write {}: {e}", cargo_config_path.display()))?;
    }

    Ok(expanded)
}

fn render_manifest_cargo_toml(
    manifest: &ScarletManifest,
    project_dir: &Path,
) -> Result<String, String> {
    let bsp = manifest_bsp_config(manifest, project_dir)?;
    let mut out = String::new();
    let _ = writeln!(&mut out, "# generated by cargo-scarlet");
    out.push_str("[package]\n");
    out.push_str("name = \"scarlet-modules\"\n");
    out.push_str("version = \"0.1.0\"\n");
    out.push_str("edition = \"2024\"\n\n");
    out.push_str("[lib]\npath = \"src/lib.rs\"\n\n");
    out.push_str("[dependencies]\n");

    let features = render_kernel_features(&bsp.kernel_features);
    let target_triple = target_triple_from_build_target(&bsp.build_target)?;
    let kernel_dep = match bsp.kernel_source {
        PackageSource::Path(p) | PackageSource::PathTable { path: p } => {
            let expanded = expand_templates(p, &target_triple, "");
            let kernel_abs = resolve_path(project_dir, &expanded);
            let generated_root = project_dir.join(".scarlet/scarlet-modules");
            let kernel_rel = pathdiff(&kernel_abs, &generated_root)?;
            format!(
                "{{ path = \"{}\", default-features = false, features = [{}] }}",
                kernel_rel.display(),
                features
            )
        }
        PackageSource::Git {
            git,
            branch,
            tag,
            rev,
        } => {
            let mut parts = vec![format!("git = \"{git}\"")];
            if let Some(r) = rev {
                parts.push(format!("rev = \"{r}\""));
            }
            if let Some(b) = branch {
                parts.push(format!("branch = \"{b}\""));
            }
            if let Some(t) = tag {
                parts.push(format!("tag = \"{t}\""));
            }
            parts.push("default-features = false".to_string());
            parts.push(format!("features = [{features}]"));
            format!("{{ {} }}", parts.join(", "))
        }
    };
    let _ = writeln!(&mut out, "{} = {}", bsp.package, kernel_dep);

    for (name, module) in &manifest.modules {
        if !module.enabled {
            continue;
        }
        let spec = render_dependency_spec(project_dir, module)?;
        let _ = writeln!(&mut out, "{name} = {{ {spec} }}");
    }

    Ok(out)
}

fn render_manifest_lib_rs(manifest: &ScarletManifest) -> String {
    let mut source = String::new();
    source.push_str("#![no_std]\n\n");
    source.push_str("pub use scarlet;\n\n");
    source.push_str("#[inline(never)]\n");
    source.push_str("pub fn force_link() {\n");
    for name in manifest
        .modules
        .keys()
        .filter(|n| manifest.modules[*n].enabled)
    {
        let identifier = cargo_key_to_rust_identifier(name);
        let _ = writeln!(&mut source, "    {identifier}::force_link();");
    }
    source.push_str("}\n");
    source
}

fn render_cargo_config() -> String {
    let mut out = String::new();
    out.push_str("# Configure build settings for the scarlet-modules workspace.\n");
    out.push_str("# This file is generated once by cargo-scarlet and will not be overwritten.\n");
    out.push_str("#\n");
    out.push_str("# Required fields:\n");
    out.push_str("#   [build]\n");
    out.push_str("#   target = \"<path-to-target-json>\"\n");
    out.push_str("#\n");
    out.push_str("#   [unstable]\n");
    out.push_str("#   build-std = [\"core\", \"compiler_builtins\", \"alloc\"]\n");
    out.push_str("#   build-std-features = [\"compiler-builtins-mem\"]\n");
    out.push_str("#\n");
    out.push_str("# Optional:\n");
    out.push_str("#   [profile.dev]\n");
    out.push_str("#   opt-level = 3\n");
    out.push_str("#\n");
    out.push_str("#   [target.<target-triple>]\n");
    out.push_str("#   rustflags = [\"-T\", \"path/to/linker.ld\"]\n");
    out.push('\n');
    out
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut hasher = Sha256::new();
    let mut f = fs::File::open(path)
        .map_err(|e| format!("failed to open {} for hashing: {e}", path.display()))?;
    let mut buf = [0u8; 8192];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn normalize_sha256(input: &str) -> Result<String, String> {
    let hex = input.strip_prefix("sha256:").unwrap_or(input);
    if hex.len() != 64 {
        return Err(format!(
            "invalid SHA-256 '{input}': expected 64 hexadecimal characters"
        ));
    }
    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "invalid SHA-256 '{input}': expected only hexadecimal characters"
        ));
    }
    Ok(format!("sha256:{}", hex.to_ascii_lowercase()))
}

fn sha256_dir(dir: &Path) -> Result<String, String> {
    let mut hasher = Sha256::new();
    sha256_dir_recursive(dir, &mut hasher)?;
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn load_lock(project_dir: &Path) -> ImageLock {
    let lock_path = project_dir.join("scarlet.lock");
    let text = match fs::read_to_string(&lock_path) {
        Ok(t) => t,
        Err(_) => return ImageLock::default(),
    };
    let image_lock: ImageLock = match toml::from_str(&text) {
        Ok(v) => v,
        Err(_) => return ImageLock::default(),
    };
    image_lock
}

fn save_lock(project_dir: &Path, lock: &ImageLock) -> Result<(), String> {
    let lock_path = project_dir.join("scarlet.lock");
    let mut text = String::from("# Generated by cargo-scarlet — do not edit\n\n");
    let lock_toml =
        toml::to_string_pretty(lock).map_err(|e| format!("failed to serialize lock: {e}"))?;
    text.push_str(&lock_toml);
    fs::write(&lock_path, &text)
        .map_err(|e| format!("failed to write {}: {e}", lock_path.display()))?;
    eprintln!("cargo-scarlet: wrote {}", lock_path.display());
    Ok(())
}

fn sha256_dir_recursive(dir: &Path, hasher: &mut Sha256) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| format!("failed to read_dir {}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        if path.is_symlink() {
            let target = fs::read_link(&path)
                .map_err(|e| format!("failed to read symlink {}: {e}", path.display()))?;
            hasher.update(
                format!("sym:{}:{}\n", entry.file_name().display(), target.display()).as_bytes(),
            );
        } else if path.is_dir() {
            hasher.update(format!("dir:{}\n", entry.file_name().display()).as_bytes());
            sha256_dir_recursive(&path, hasher)?;
        } else {
            let mut f = fs::File::open(&path)
                .map_err(|e| format!("failed to open {}: {e}", path.display()))?;
            let mut content = Vec::new();
            f.read_to_end(&mut content)
                .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
            let file_hash = format!("{:x}", sha2::Sha256::digest(&content));
            hasher
                .update(format!("file:{}:{}\n", entry.file_name().display(), file_hash).as_bytes());
        }
    }
    Ok(())
}

fn cmd_update(project: &Path, offline: bool) -> Result<(), String> {
    let mut expanded = expand_manifest_with_offline(project, offline)?;
    let git_cache_dir = project.join(".scarlet/cache/git");
    let file_cache_dir = project.join(".scarlet/cache/files");
    let mut lock = load_lock(project);

    for section in expanded.sections.values_mut() {
        for pkg in section.packages_mut() {
            if let Some(ref src) = pkg.source
                && src.is_git()
            {
                let (checkout, rev) = resolve_git_source(src, &git_cache_dir, None, offline)?;
                pkg.local_source = Some(checkout);
                pkg.resolved_rev = Some(rev.clone());
            }
        }
    }

    for (section_name, section) in &expanded.sections {
        let section_lock = lock
            .sections
            .entry(section_name.clone())
            .or_insert_with(|| SectionLock {
                hash: String::new(),
                layers: Vec::new(),
                files: Vec::new(),
                packages: Vec::new(),
            });

        let previous_section_lock = section_lock.clone();
        let mut layers = Vec::new();
        for layer in &section.layers {
            match layer {
                ResolvedLayer::Copy(file) => {
                    if let FileSource::Url(url) = &file.source {
                        eprintln!("cargo-scarlet: fetching {}", url);
                        let previous_hash = previous_section_lock
                            .copy_locks()
                            .find(|lock| copy_lock_matches_input(lock, file))
                            .or_else(|| {
                                previous_section_lock
                                    .copy_locks()
                                    .find(|lock| lock.source == *url)
                            })
                            .map(|lock| lock.hash);
                        let (_, hash) = fetch_url_cached_with_offline(
                            url,
                            &file_cache_dir,
                            previous_hash.as_deref(),
                            offline,
                        )?;
                        layers.push(LayerLock::Copy {
                            source: url.clone(),
                            to: file.to.clone(),
                            template: file.template,
                            hash,
                        });
                    }
                }
                ResolvedLayer::Archive(archive) => {
                    fetch_archive_by_sha256(
                        &archive.url,
                        &archive.sha256,
                        &file_cache_dir,
                        offline,
                    )?;
                    layers.push(LayerLock::Archive {
                        source: LockPackageSource::archive(
                            archive.url.clone(),
                            archive.sha256.clone(),
                        ),
                        to: archive.to.clone(),
                        format: archive.format.clone(),
                        strip_components: archive.strip_components,
                        hash: archive.sha256.clone(),
                    });
                }
                ResolvedLayer::Package(pkg) => {
                    let previous_hash =
                        find_package_lock_for_input(project, &previous_section_lock, pkg)?
                            .map(|lock| lock.hash)
                            .unwrap_or_default();
                    layers.push(package_lock_to_layer(package_input_lock(
                        project,
                        pkg,
                        previous_hash,
                    )?));
                }
                ResolvedLayer::Image { .. } => {}
            }
        }
        section_lock.layers = layers;
    }

    let section_names: Vec<String> = expanded.sections.keys().cloned().collect();
    lock.sections.retain(|name, _| section_names.contains(name));

    save_lock(project, &lock)?;
    eprintln!("cargo-scarlet: lock updated");
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cargo-scarlet: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse_from(normalized_args());
    match cli.command {
        Commands::Check {
            project,
            target,
            release,
        } => {
            let project = normalize_project_path(&project)?;
            let expanded = generate_from_manifest(&project)?;
            cargo_build_manifest(
                &project,
                &expanded,
                target.as_deref(),
                release,
                "check",
                &[],
            )
        }
        Commands::Build {
            project,
            target,
            release,
            module,
            output,
            locked,
            offline,
        } => {
            if let Some(module_path) = module {
                build_loadable_module(&module_path, target.as_deref(), output.as_deref(), release)?;
                Ok(())
            } else {
                let project = project.ok_or("--project is required when not using --module")?;
                let project = normalize_project_path(&project)?;
                let expanded = generate_from_manifest_with_offline(&project, offline)?;
                if locked {
                    validate_locked_archive_layers(&expanded, &load_lock(&project))?;
                }
                cargo_build_manifest(
                    &project,
                    &expanded,
                    target.as_deref(),
                    release,
                    "build",
                    &[],
                )?;
                inject_ksym_section_manifest(&project, &expanded, target.as_deref(), release)
            }
        }
        Commands::Clippy {
            project,
            target,
            release,
            extra_args,
        } => {
            let project = normalize_project_path(&project)?;
            let expanded = generate_from_manifest(&project)?;
            cargo_build_manifest(
                &project,
                &expanded,
                target.as_deref(),
                release,
                "clippy",
                &extra_args,
            )
        }
        Commands::Run {
            project,
            target,
            release,
            no_image,
            locked,
            offline,
            extra_args,
        } => {
            let project = normalize_project_path(&project)?;
            let expanded = generate_from_manifest_with_offline(&project, offline)?;
            if locked {
                validate_locked_archive_layers(&expanded, &load_lock(&project))?;
            }

            if !no_image {
                build_manifest_image(&project, target, release, None, false, locked, offline)?;
            }

            match &expanded.manifest.runner {
                Some(runner) => {
                    let runner_path = if Path::new(&runner.command).is_absolute() {
                        PathBuf::from(&runner.command)
                    } else {
                        project.join(&runner.command)
                    };

                    let mut cmd = Command::new(&runner_path);
                    cmd.current_dir(&project);
                    if release {
                        cmd.env("SCARLET_RELEASE", "1");
                    }
                    cmd.args(&extra_args);

                    let status = cmd
                        .status()
                        .map_err(|e| format!("failed to run runner: {e}"))?;

                    if status.success() {
                        Ok(())
                    } else {
                        Err("runner exited with non-zero status".to_string())
                    }
                }
                None => {
                    Err("no [runner] defined in scarlet.toml; running is not supported for this project".to_string())
                }
            }
        }
        Commands::Image {
            project,
            target,
            release,
            kernel_elf,
            no_build,
            locked,
            offline,
        } => {
            let project = normalize_project_path(&project)?;
            build_manifest_image(
                &project, target, release, kernel_elf, no_build, locked, offline,
            )
        }
        Commands::New {
            module,
            project,
            kernel_path,
            kernel_rev,
            target,
        } => new_scaffold(
            module,
            project,
            kernel_path.as_deref(),
            kernel_rev.as_deref(),
            target.as_deref(),
        ),
        Commands::Update { project, offline } => {
            let project = normalize_project_path(&project)?;
            cmd_update(&project, offline)
        }
    }
}

fn cargo_build_manifest(
    project: &Path,
    expanded: &ExpandedManifest,
    target: Option<&str>,
    release: bool,
    subcommand: &str,
    extra_args: &[String],
) -> Result<(), String> {
    let bsp = manifest_bsp_config(&expanded.manifest, project)?;
    let resolved_target = target
        .map(str::to_string)
        .unwrap_or_else(|| bsp.build_target.clone());
    let target_arg = build_target_arg(&bsp.root, &resolved_target);

    metadata_check(
        &bsp.root,
        &target_arg,
        bsp.package,
        &bsp.disabled_kernel_features,
    )?;

    let mut command = Command::new("cargo");
    command.arg(subcommand);
    if release {
        command.arg("--release");
    }
    command.arg("--target").arg(&target_arg);

    if subcommand == "clippy" && !extra_args.iter().any(|arg| arg == "--") {
        command.arg("--").arg("-D").arg("warnings");
    }

    command.args(extra_args);
    command.current_dir(&bsp.root);

    eprintln!(
        "cargo-scarlet: running in {} -> cargo {}",
        bsp.root.display(),
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    );

    let status = command
        .status()
        .map_err(|e| format!("failed to run cargo {subcommand}: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo {subcommand} failed with status {status}"))
    }
}

fn inject_ksym_section_manifest(
    project: &Path,
    expanded: &ExpandedManifest,
    target: Option<&str>,
    release: bool,
) -> Result<(), String> {
    let bsp = manifest_bsp_config(&expanded.manifest, project)?;
    let resolved_target = match target {
        Some(t) => t.to_string(),
        None => bsp.build_target.clone(),
    };
    let target_triple = target_triple_from_build_target(&resolved_target)?;

    let profile = if release { "release" } else { "debug" };
    let binary_path = bsp
        .root
        .join("target")
        .join(&target_triple)
        .join(profile)
        .join("scarlet");

    if !binary_path.exists() {
        eprintln!(
            "cargo-scarlet: ksym: binary not found at {}, skipping",
            binary_path.display()
        );
        return Ok(());
    }

    let (nm_cmd, objcopy_cmd) = cross_tools_for_target(&target_triple);

    let nm_output = Command::new(&nm_cmd)
        .args([
            "--defined-only",
            "--extern-only",
            "-g",
            "--no-sort",
            binary_path.to_str().unwrap_or(""),
        ])
        .output()
        .map_err(|e| format!("failed to run nm: {e}"))?;

    if !nm_output.status.success() {
        eprintln!("cargo-scarlet: ksym: nm failed, skipping section injection");
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&nm_output.stdout);
    let mut symbols: Vec<(u64, String)> = Vec::new();

    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }
        let addr_str = parts[0];
        let name = parts[2];

        if name.is_empty() {
            continue;
        }

        let skip = match name {
            "_GLOBAL_OFFSET_TABLE_" | "_DYNAMIC" => true,
            n if n.starts_with("__") && n.ends_with("_START") => true,
            n if n.starts_with("__") && n.ends_with("_END") => true,
            _ => false,
        };

        if skip {
            continue;
        }

        let addr = u64::from_str_radix(addr_str, 16).unwrap_or(0);
        symbols.push((addr, name.to_string()));
    }

    let count = symbols.len() as u64;
    let mut blob = Vec::new();
    blob.extend_from_slice(&count.to_le_bytes());

    for (addr, name) in &symbols {
        blob.extend_from_slice(&addr.to_le_bytes());
        let name_len = name.len() as u64;
        blob.extend_from_slice(&name_len.to_le_bytes());
        blob.extend_from_slice(name.as_bytes());
    }

    let tmp_dir = std::env::temp_dir().join("scarlet-ksym");
    fs::create_dir_all(&tmp_dir).map_err(|e| format!("failed to create temp dir: {e}"))?;
    let blob_path = tmp_dir.join("ksym_blob.bin");

    fs::write(&blob_path, &blob).map_err(|e| format!("failed to write ksym blob: {e}"))?;

    let objcopy_status = Command::new(&objcopy_cmd)
        .args([
            "--add-section",
            &format!(".ksym={}", blob_path.display()),
            "--set-section-flags",
            ".ksym=alloc,readonly",
            binary_path.to_str().unwrap_or(""),
        ])
        .status()
        .map_err(|e| format!("failed to run objcopy: {e}"))?;

    if objcopy_status.success() {
        eprintln!("cargo-scarlet: ksym: injected {} symbols", symbols.len());
        Ok(())
    } else {
        eprintln!("cargo-scarlet: ksym: objcopy failed, skipping section injection");
        Ok(())
    }
}

fn build_manifest_image(
    project: &Path,
    target: Option<String>,
    release: bool,
    kernel_elf: Option<PathBuf>,
    no_build: bool,
    locked: bool,
    offline: bool,
) -> Result<(), String> {
    let mut expanded = generate_from_manifest_with_offline(project, offline)?;
    let existing_lock = load_lock(project);
    if locked {
        validate_locked_archive_layers(&expanded, &existing_lock)?;
    }

    if !no_build {
        cargo_build_manifest(project, &expanded, target.as_deref(), release, "build", &[])?;
        inject_ksym_section_manifest(project, &expanded, target.as_deref(), release)?;
    }

    let kernel_elf = match kernel_elf {
        Some(path) => absolutize_from_current_dir(&path)?,
        None => {
            let bsp = manifest_bsp_config(&expanded.manifest, project)?;
            let build_target = target.as_deref().unwrap_or(&bsp.build_target);
            let target_triple = target_triple_from_build_target(build_target)?;
            let profile = if release { "release" } else { "debug" };
            let path = bsp
                .root
                .join("target")
                .join(&target_triple)
                .join(profile)
                .join("scarlet");
            if !path.exists() {
                return Err(format!("kernel ELF not found: {}", path.display()));
            }
            path
        }
    };

    let images_dir = project.join(".scarlet/images");
    fs::create_dir_all(&images_dir)
        .map_err(|e| format!("failed to create {}: {e}", images_dir.display()))?;

    let bsp = manifest_bsp_config(&expanded.manifest, project)?;
    let build_target = target.as_deref().unwrap_or(&bsp.build_target);
    let target_triple = target_triple_from_build_target(build_target)?;
    let profile = if release { "release" } else { "debug" };

    let raw_arch = target_triple.split('-').next().unwrap_or("unknown");
    let arch = match raw_arch {
        v if v.starts_with("riscv64") => "riscv64".to_string(),
        v if v.starts_with("riscv32") => "riscv32".to_string(),
        v if v.starts_with("aarch64") => "aarch64".to_string(),
        v => v.to_string(),
    };
    let tpl_ctx = TemplateContext {
        arch,
        target_triple: target_triple.clone(),
        project: expanded.manifest.project.name.clone(),
    };

    let build_order = topo_sort_images(&expanded.manifest.images)?;

    resolve_git_sources(&mut expanded, project, &existing_lock, offline)?;
    let mut new_lock = ImageLock::default();

    for section_name in build_order {
        let section_cfg = expanded
            .manifest
            .images
            .get(&section_name)
            .ok_or_else(|| format!("section '{}' not in manifest", section_name))?;
        let resolved = expanded
            .sections
            .get(&section_name)
            .ok_or_else(|| format!("section '{}' not resolved", section_name))?;

        let output = section_cfg
            .output
            .as_deref()
            .unwrap_or(".scarlet/images/output");
        let output_path = project.join(output);
        let staging_dir = project.join(format!(".scarlet/staging/{}", section_name));
        let format = section_cfg.format.as_deref().unwrap_or("");

        eprintln!("cargo-scarlet: staging {}...", section_name);

        match format {
            "newc" | "ext2" | "gpt-ext2" => {
                if staging_dir.exists() {
                    fs::remove_dir_all(&staging_dir)
                        .map_err(|e| format!("failed to clean staging: {e}"))?;
                }
                fs::create_dir_all(&staging_dir)
                    .map_err(|e| format!("failed to create staging: {e}"))?;

                let cache_dir = project.join(".scarlet/cache/files");
                let prev_section_lock = existing_lock.sections.get(&section_name);
                let mut layer_locks: Vec<LayerLock> = Vec::new();

                for layer in &resolved.layers {
                    match layer {
                        ResolvedLayer::Copy(file) => {
                            apply_copy_layer(
                                file,
                                &staging_dir,
                                &cache_dir,
                                prev_section_lock,
                                &tpl_ctx,
                                offline,
                                &mut layer_locks,
                            )?;
                        }
                        ResolvedLayer::Archive(archive) => {
                            apply_archive_layer(
                                archive,
                                &staging_dir,
                                &cache_dir,
                                offline,
                                &mut layer_locks,
                            )?;
                        }
                        ResolvedLayer::Package(pkg) => {
                            let prev_pkg =
                                existing_lock.sections.get(&section_name).and_then(|s| {
                                    s.package_locks().find(|p| {
                                        package_lock_matches_input(project, p, pkg).unwrap_or(false)
                                    })
                                });
                            if let Some(lock) = install_package(
                                &staging_dir,
                                pkg,
                                project,
                                &target_triple,
                                profile,
                                prev_pkg.as_ref(),
                            )? {
                                layer_locks.push(package_lock_to_layer(lock));
                            }
                        }
                        ResolvedLayer::Image { source, to } => {
                            let from_staging = project.join(format!(".scarlet/staging/{}", source));
                            let dest = staging_dir.join(to.trim_start_matches('/'));
                            if from_staging.is_dir() {
                                copy_dir_recursive(&from_staging, &dest)?;
                            } else {
                                let image_path =
                                    image_output_path(project, &expanded.manifest.images, source)?;
                                copy_path_or_dir(&image_path, &dest)?;
                            }
                        }
                    }
                }

                let staging_hash = sha256_dir(&staging_dir)?;
                let image_hash = image_content_hash(format, &staging_hash);

                let existing_section_lock = existing_lock.sections.get(&section_name);
                if image_output_is_current(
                    project,
                    &section_name,
                    &output_path,
                    &image_hash,
                    existing_section_lock,
                ) {
                    eprintln!(
                        "cargo-scarlet: {} unchanged, skipping image generation",
                        section_name
                    );
                    let mut updated = existing_section_lock.unwrap().clone();
                    updated.layers = layer_locks;
                    updated.packages = Vec::new();
                    updated.files = Vec::new();
                    new_lock.sections.insert(section_name.clone(), updated);
                    continue;
                }

                eprintln!("cargo-scarlet: generating {} image...", section_name);

                match format {
                    "newc" => {
                        build_initramfs_newc_from_staging(&staging_dir, &output_path)?;
                        eprintln!(
                            "cargo-scarlet: wrote {} to {}",
                            section_name,
                            output_path.display()
                        );
                    }
                    "ext2" => {
                        build_ext2_from_staging(&staging_dir, &output_path, &section_name)?;
                    }
                    "gpt-ext2" => {
                        build_gpt_ext2_from_staging(&staging_dir, &output_path, &section_name)?;
                    }
                    _ => unreachable!(),
                }

                write_image_stamp(project, &section_name, &output_path, &image_hash)?;

                new_lock.sections.insert(
                    section_name.clone(),
                    SectionLock {
                        hash: image_hash,
                        layers: layer_locks,
                        files: Vec::new(),
                        packages: Vec::new(),
                    },
                );
            }
            "gpt" => {
                let gpt_hash = gpt_image_hash(project, &expanded.manifest.images, section_cfg)?;

                let existing_section_lock = existing_lock.sections.get(&section_name);
                if image_output_is_current(
                    project,
                    &section_name,
                    &output_path,
                    &gpt_hash,
                    existing_section_lock,
                ) {
                    eprintln!("cargo-scarlet: {} unchanged, skipping", section_name);
                    new_lock
                        .sections
                        .insert(section_name.clone(), existing_section_lock.unwrap().clone());
                    continue;
                }

                eprintln!("cargo-scarlet: building {}...", section_name);
                build_gpt_image_from_partitions(
                    project,
                    &expanded.manifest.images,
                    &section_cfg.partitions,
                    &output_path,
                    &section_name,
                )?;

                write_image_stamp(project, &section_name, &output_path, &gpt_hash)?;

                new_lock.sections.insert(
                    section_name.clone(),
                    SectionLock {
                        hash: gpt_hash,
                        layers: Vec::new(),
                        files: Vec::new(),
                        packages: Vec::new(),
                    },
                );
            }
            "limine-uefi" => {
                let arch_name = match target_triple.split('-').next() {
                    Some("aarch64") => "aarch64",
                    Some(v) if v.starts_with("riscv64") => "riscv64",
                    _ => &target_triple,
                };

                let initramfs_path =
                    initramfs_path_from_layers(project, &expanded.manifest.images, resolved)
                        .unwrap_or_else(|| project.join(".scarlet/images/initramfs.cpio"));

                let dtb_path = section_cfg
                    .dtb
                    .as_deref()
                    .map(|dtb| resolve_path(project, dtb));
                let packages =
                    plugin_packages_from_layers(project, &expanded.manifest.images, resolved)?;
                let limine_hash = limine_image_hash(
                    &section_cfg.cmdline,
                    &kernel_elf,
                    &initramfs_path,
                    dtb_path.as_deref(),
                    &packages,
                )?;

                let existing_section_lock = existing_lock.sections.get(&section_name);
                if image_output_is_current(
                    project,
                    &section_name,
                    &output_path,
                    &limine_hash,
                    existing_section_lock,
                ) {
                    eprintln!("cargo-scarlet: {} unchanged, skipping", section_name);
                    new_lock
                        .sections
                        .insert(section_name.clone(), existing_section_lock.unwrap().clone());
                    continue;
                }

                eprintln!("cargo-scarlet: building {}...", section_name);

                if let Some(parent) = output_path.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
                }

                let request = PluginRequest {
                    project_dir: project.display().to_string(),
                    section_name: &section_name,
                    format,
                    arch: arch_name.to_string(),
                    kernel_elf: kernel_elf.display().to_string(),
                    initramfs: Some(initramfs_path.display().to_string()),
                    output: output_path.display().to_string(),
                    section: PluginRequestSection {
                        cmdline: Some(section_cfg.cmdline.clone()),
                        dtb: dtb_path.as_ref().map(|path| path.display().to_string()),
                        packages,
                    },
                };
                run_plugin("limine", &request)?;

                eprintln!(
                    "cargo-scarlet: wrote {} to {}",
                    section_name,
                    output_path.display()
                );

                write_image_stamp(project, &section_name, &output_path, &limine_hash)?;

                new_lock.sections.insert(
                    section_name.clone(),
                    SectionLock {
                        hash: limine_hash,
                        layers: Vec::new(),
                        files: Vec::new(),
                        packages: Vec::new(),
                    },
                );
            }
            _ => {
                return Err(format!(
                    "unsupported format '{}' for section '{}'",
                    format, section_name
                ));
            }
        }
    }

    eprintln!(
        "cargo-scarlet: saving lock with {} sections",
        new_lock.sections.len()
    );
    save_lock(project, &new_lock)?;

    Ok(())
}

fn run_plugin<T: Serialize>(name: &str, request: &T) -> Result<(), String> {
    let program = format!("cargo-scarlet-plugin-{name}");
    let payload = serde_json::to_vec(request)
        .map_err(|error| format!("failed to encode plugin request for '{name}': {error}"))?;
    let mut child = Command::new(&program)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to run plugin '{name}' ({program}): {error}"))?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| format!("failed to open stdin for plugin '{name}'"))?
        .write_all(&payload)
        .map_err(|error| format!("failed to write plugin request for '{name}': {error}"))?;
    let status = child
        .wait()
        .map_err(|error| format!("failed to wait for plugin '{name}': {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("plugin '{name}' failed with status {status}"))
    }
}

fn topo_sort_images(
    images: &BTreeMap<String, ManifestImageSection>,
) -> Result<Vec<String>, String> {
    let mut in_degree: BTreeMap<String, usize> = BTreeMap::new();
    let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for name in images.keys() {
        in_degree.insert(name.clone(), 0);
        dependents.insert(name.clone(), Vec::new());
    }

    for (name, section) in images {
        for dep in &section.deps {
            if !images.contains_key(dep) {
                return Err(format!(
                    "image '{}' depends on unknown image '{}'",
                    name, dep
                ));
            }
            *in_degree.entry(name.clone()).or_insert(0) += 1;
            dependents
                .entry(dep.clone())
                .or_default()
                .push(name.clone());
        }
    }

    let mut queue: Vec<String> = in_degree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(name, _)| name.clone())
        .collect();

    let mut result = Vec::new();
    while let Some(name) = queue.pop() {
        result.push(name.clone());
        for dep in dependents.get(&name).unwrap_or(&Vec::new()) {
            let degree = in_degree.get_mut(dep).unwrap();
            *degree -= 1;
            if *degree == 0 {
                queue.push(dep.clone());
            }
        }
    }

    if result.len() != images.len() {
        return Err("circular dependency detected in images".to_string());
    }

    Ok(result)
}

fn image_content_hash(format: &str, staging_hash: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("format={format}\n").as_bytes());
    hasher.update(format!("staging={staging_hash}\n").as_bytes());
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn limine_image_hash(
    cmdline: &str,
    kernel_elf: &Path,
    initramfs_path: &Path,
    dtb_path: Option<&Path>,
    packages: &[PluginRequestPackage],
) -> Result<String, String> {
    let mut hasher = Sha256::new();
    hasher.update(b"format=limine-uefi\n");
    hasher.update(format!("cmdline={cmdline}\n").as_bytes());
    if kernel_elf.exists() {
        hasher.update(
            format!(
                "kernel:{}:{}\n",
                kernel_elf.display(),
                sha256_file(kernel_elf)?
            )
            .as_bytes(),
        );
    }
    if initramfs_path.exists() {
        hasher.update(
            format!(
                "initramfs:{}:{}\n",
                initramfs_path.display(),
                sha256_file(initramfs_path)?
            )
            .as_bytes(),
        );
    }
    if let Some(dtb_path) = dtb_path {
        hasher
            .update(format!("dtb:{}:{}\n", dtb_path.display(), sha256_file(dtb_path)?).as_bytes());
    }
    for package in packages {
        let source = Path::new(&package.source);
        if !source.is_file() {
            return Err(format!(
                "Limine boot package source must be a file: {}",
                source.display()
            ));
        }
        hasher.update(
            format!(
                "package:{}:{}:{}\n",
                package.to,
                source.display(),
                sha256_file(source)?
            )
            .as_bytes(),
        );
    }
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn image_output_is_current(
    project: &Path,
    section_name: &str,
    output_path: &Path,
    expected_hash: &str,
    existing_lock: Option<&SectionLock>,
) -> bool {
    output_path.exists()
        && existing_lock.is_some_and(|existing| existing.hash == expected_hash)
        && image_stamp_matches(project, section_name, output_path, expected_hash)
}

fn image_stamp_matches(
    project: &Path,
    section_name: &str,
    output_path: &Path,
    expected_hash: &str,
) -> bool {
    let Ok(text) = fs::read_to_string(image_stamp_path(project, section_name)) else {
        return false;
    };

    let expected_output = output_path.display().to_string();
    let mut hash_matches = false;
    let mut output_matches = false;
    for line in text.lines() {
        if let Some(hash) = line.strip_prefix("hash = ") {
            hash_matches = hash == expected_hash;
        } else if let Some(output) = line.strip_prefix("output = ") {
            output_matches = output == expected_output;
        }
    }
    hash_matches && output_matches
}

fn write_image_stamp(
    project: &Path,
    section_name: &str,
    output_path: &Path,
    hash: &str,
) -> Result<(), String> {
    let stamp_path = image_stamp_path(project, section_name);
    if let Some(parent) = stamp_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    let text = format!("hash = {hash}\noutput = {}\n", output_path.display());
    fs::write(&stamp_path, text)
        .map_err(|e| format!("failed to write {}: {e}", stamp_path.display()))
}

fn image_stamp_path(project: &Path, section_name: &str) -> PathBuf {
    let safe_name: String = section_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    project
        .join(".scarlet/image-stamps")
        .join(format!("{safe_name}.stamp"))
}

fn gpt_image_hash(
    project: &Path,
    images: &BTreeMap<String, ManifestImageSection>,
    section: &ManifestImageSection,
) -> Result<String, String> {
    let mut hasher = Sha256::new();
    hasher.update(b"format=gpt\n");
    for partition in &section.partitions {
        let source_path = image_output_path(project, images, &partition.source)?;
        hasher.update(format!("source={}\n", partition.source).as_bytes());
        hasher.update(format!("path={}\n", source_path.display()).as_bytes());
        hasher.update(format!("hash={}\n", sha256_file(&source_path)?).as_bytes());
        hasher.update(format!("name={}\n", partition.name).as_bytes());
        hasher.update(format!("type={}\n", partition.type_name).as_bytes());
        hasher.update(format!("flags={}\n", partition.flags).as_bytes());
        hasher.update(format!("alignment_lba={:?}\n", partition.alignment_lba).as_bytes());
    }
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn build_gpt_image_from_partitions(
    project: &Path,
    images: &BTreeMap<String, ManifestImageSection>,
    partitions: &[ManifestGptPartition],
    output_path: &Path,
    section_name: &str,
) -> Result<(), String> {
    if partitions.is_empty() {
        return Err(format!(
            "GPT image section '{section_name}' has no partitions"
        ));
    }

    let output_parent = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(output_parent)
        .map_err(|e| format!("failed to create {}: {e}", output_parent.display()))?;

    let mut planned = Vec::new();
    let mut next_lba = GPT_FIRST_PARTITION_LBA;
    for (index, partition) in partitions.iter().enumerate() {
        let source_path = image_output_path(project, images, &partition.source)?;
        let size = file_size(&source_path)?;
        if size == 0 {
            return Err(format!(
                "GPT partition '{}' source {} is empty",
                partition.name,
                source_path.display()
            ));
        }

        let alignment_lba = partition.alignment_lba.unwrap_or(GPT_FIRST_PARTITION_LBA);
        if alignment_lba == 0 {
            return Err(format!(
                "GPT partition '{}' alignment_lba must be greater than zero",
                partition.name
            ));
        }
        let length_lba = size.div_ceil(GPT_SECTOR_SIZE);
        let first_lba = align_up(next_lba, alignment_lba)?;
        let last_lba = first_lba
            .checked_add(length_lba)
            .and_then(|value| value.checked_sub(1))
            .ok_or("GPT partition LBA range overflow")?;
        planned.push(PlannedGptPartition {
            id: u32::try_from(index + 1).map_err(|_| "too many GPT partitions")?,
            source_path,
            name: partition.name.clone(),
            part_type: gpt_partition_type(&partition.type_name)?,
            flags: partition.flags,
            first_lba,
            length_lba,
            size,
        });
        next_lba = last_lba
            .checked_add(1)
            .ok_or("GPT disk LBA count overflow")?;
    }

    let disk_lbas = next_lba
        .checked_add(GPT_TRAILING_PADDING_LBAS)
        .ok_or("GPT disk LBA count overflow")?;
    let disk_size = disk_lbas
        .checked_mul(GPT_SECTOR_SIZE)
        .ok_or("GPT disk size overflow")?;

    let _ = fs::remove_file(output_path);
    let disk_file = fs::File::create(output_path)
        .map_err(|e| format!("failed to create {}: {e}", output_path.display()))?;
    disk_file
        .set_len(disk_size)
        .map_err(|e| format!("failed to size {}: {e}", output_path.display()))?;
    drop(disk_file);

    let mut disk_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(output_path)
        .map_err(|e| format!("failed to open {}: {e}", output_path.display()))?;
    let pmbr_size = u32::try_from(disk_lbas.saturating_sub(1)).unwrap_or(u32::MAX);
    let pmbr = gpt::mbr::ProtectiveMBR::with_lb_size(pmbr_size);
    pmbr.overwrite_lba0(&mut disk_file)
        .map_err(|e| format!("failed to write protective MBR: {e}"))?;

    let mut gpt_disk = gpt::GptConfig::new()
        .writable(true)
        .logical_block_size(gpt::disk::LogicalBlockSize::Lb512)
        .create_from_device(disk_file, None)
        .map_err(|e| format!("failed to create GPT: {e}"))?;
    for partition in &planned {
        gpt_disk
            .add_partition_at(
                &partition.name,
                partition.id,
                partition.first_lba,
                partition.length_lba,
                partition.part_type.clone(),
                partition.flags,
            )
            .map_err(|e| format!("failed to add GPT partition '{}': {e}", partition.name))?;
    }
    let mut disk_file = gpt_disk
        .write()
        .map_err(|e| format!("failed to write GPT: {e}"))?;

    for partition in &planned {
        disk_file
            .seek(SeekFrom::Start(partition.first_lba * GPT_SECTOR_SIZE))
            .map_err(|e| format!("failed to seek {}: {e}", output_path.display()))?;
        let mut source = fs::File::open(&partition.source_path)
            .map_err(|e| format!("failed to open {}: {e}", partition.source_path.display()))?;
        std::io::copy(&mut source, &mut disk_file).map_err(|e| {
            format!(
                "failed to copy {} into GPT image: {e}",
                partition.source_path.display()
            )
        })?;
        eprintln!(
            "cargo-scarlet: {} p{} {} first_lba={} size={}KB",
            section_name,
            partition.id,
            partition.name,
            partition.first_lba,
            partition.size.div_ceil(1024)
        );
    }
    disk_file
        .sync_all()
        .map_err(|e| format!("failed to sync {}: {e}", output_path.display()))?;

    eprintln!(
        "cargo-scarlet: wrote {} to {} (GPT, {} partitions, {}KB)",
        section_name,
        output_path.display(),
        planned.len(),
        disk_size.div_ceil(1024)
    );
    Ok(())
}

struct PlannedGptPartition {
    id: u32,
    source_path: PathBuf,
    name: String,
    part_type: gpt::partition_types::Type,
    flags: u64,
    first_lba: u64,
    length_lba: u64,
    size: u64,
}

fn align_up(value: u64, alignment: u64) -> Result<u64, String> {
    if alignment == 0 {
        return Err("alignment must be greater than zero".to_string());
    }
    let remainder = value % alignment;
    if remainder == 0 {
        Ok(value)
    } else {
        value
            .checked_add(alignment - remainder)
            .ok_or("alignment overflow".to_string())
    }
}

fn gpt_partition_type(type_name: &str) -> Result<gpt::partition_types::Type, String> {
    let normalized = type_name.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "efi" | "efi-system" | "esp" => Ok(gpt::partition_types::EFI),
        "linux" | "linux-filesystem" | "linux-fs" => Ok(gpt::partition_types::LINUX_FS),
        "basic" | "basic-data" => Ok(gpt::partition_types::BASIC),
        other => Err(format!("unsupported GPT partition type '{other}'")),
    }
}

fn build_gpt_ext2_from_staging(
    staging_dir: &Path,
    output_path: &Path,
    section_name: &str,
) -> Result<(), String> {
    let output_parent = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(output_parent)
        .map_err(|e| format!("failed to create {}: {e}", output_parent.display()))?;

    let output_name = output_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("image");
    let work_dir = output_parent.join(format!(
        ".{output_name}.gpt-ext2-work.{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&work_dir);
    fs::create_dir_all(&work_dir)
        .map_err(|e| format!("failed to create {}: {e}", work_dir.display()))?;

    let result = (|| {
        let partition_image = work_dir.join("rootfs.ext2");
        build_ext2_from_staging(staging_dir, &partition_image, section_name)?;

        let partition_size = file_size(&partition_image)?;
        let partition_lbas = partition_size.div_ceil(GPT_SECTOR_SIZE);
        if partition_lbas == 0 {
            return Err("generated ext2 partition image is empty".to_string());
        }

        let first_lba = GPT_FIRST_PARTITION_LBA;
        let last_lba = first_lba
            .checked_add(partition_lbas)
            .and_then(|value| value.checked_sub(1))
            .ok_or("GPT partition LBA range overflow")?;
        let disk_lbas = last_lba
            .checked_add(1)
            .and_then(|value| value.checked_add(GPT_TRAILING_PADDING_LBAS))
            .ok_or("GPT disk LBA count overflow")?;
        let disk_size = disk_lbas
            .checked_mul(GPT_SECTOR_SIZE)
            .ok_or("GPT disk size overflow")?;

        let _ = fs::remove_file(output_path);
        let disk_file = fs::File::create(output_path)
            .map_err(|e| format!("failed to create {}: {e}", output_path.display()))?;
        disk_file
            .set_len(disk_size)
            .map_err(|e| format!("failed to size {}: {e}", output_path.display()))?;
        drop(disk_file);

        let mut disk_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(output_path)
            .map_err(|e| format!("failed to open {}: {e}", output_path.display()))?;
        let pmbr_size = u32::try_from(disk_lbas.saturating_sub(1)).unwrap_or(u32::MAX);
        let pmbr = gpt::mbr::ProtectiveMBR::with_lb_size(pmbr_size);
        pmbr.overwrite_lba0(&mut disk_file)
            .map_err(|e| format!("failed to write protective MBR: {e}"))?;

        let mut gpt_disk = gpt::GptConfig::new()
            .writable(true)
            .logical_block_size(gpt::disk::LogicalBlockSize::Lb512)
            .create_from_device(disk_file, None)
            .map_err(|e| format!("failed to create GPT: {e}"))?;
        gpt_disk
            .add_partition_at(
                "SCARLET_ROOT",
                1,
                first_lba,
                partition_lbas,
                gpt::partition_types::LINUX_FS,
                0,
            )
            .map_err(|e| format!("failed to add GPT partition: {e}"))?;
        let mut disk_file = gpt_disk
            .write()
            .map_err(|e| format!("failed to write GPT: {e}"))?;

        disk_file
            .seek(SeekFrom::Start(first_lba * GPT_SECTOR_SIZE))
            .map_err(|e| format!("failed to seek {}: {e}", output_path.display()))?;
        let mut partition_file = fs::File::open(&partition_image)
            .map_err(|e| format!("failed to open {}: {e}", partition_image.display()))?;
        std::io::copy(&mut partition_file, &mut disk_file)
            .map_err(|e| format!("failed to copy ext2 partition into GPT image: {e}"))?;
        disk_file
            .sync_all()
            .map_err(|e| format!("failed to sync {}: {e}", output_path.display()))?;

        eprintln!(
            "cargo-scarlet: wrote {} to {} (GPT, p1={}KB, offset={}KB)",
            section_name,
            output_path.display(),
            partition_size.div_ceil(1024),
            (first_lba * GPT_SECTOR_SIZE) / 1024
        );
        Ok(())
    })();

    let _ = fs::remove_dir_all(&work_dir);
    result
}

const GPT_SECTOR_SIZE: u64 = 512;
const GPT_FIRST_PARTITION_LBA: u64 = 2048;
const GPT_TRAILING_PADDING_LBAS: u64 = 2048;

fn build_ext2_from_staging(
    staging_dir: &Path,
    output_path: &Path,
    section_name: &str,
) -> Result<(), String> {
    let source_kb_output = Command::new("du")
        .args(["-sk", staging_dir.to_str().unwrap_or("")])
        .output()
        .map_err(|e| format!("failed to run du: {e}"))?;
    let source_kb_str = String::from_utf8_lossy(&source_kb_output.stdout);
    let source_kb: u64 = source_kb_str
        .split_whitespace()
        .next()
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    let extra_kb: u64 = 65536;
    let size_kb = (source_kb + source_kb / 3 + extra_kb).div_ceil(16384) * 16384;

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }

    let _ = fs::remove_file(output_path);
    let truncate_status = Command::new("truncate")
        .args([
            "-s",
            &format!("{size_kb}K"),
            output_path.to_str().unwrap_or(""),
        ])
        .status()
        .map_err(|e| format!("failed to run truncate: {e}"))?;
    if !truncate_status.success() {
        return Err("truncate failed".to_string());
    }

    let mke2fs_status = Command::new("mke2fs")
        .args([
            "-q",
            "-F",
            "-t",
            "ext2",
            "-b",
            "4096",
            "-i",
            "2048",
            "-m",
            "1",
            "-L",
            "SCARLET_ROOT",
            "-E",
            "no_copy_xattrs",
            "-d",
            staging_dir.to_str().unwrap_or(""),
            output_path.to_str().unwrap_or(""),
        ])
        .status()
        .map_err(|e| format!("failed to run mke2fs: {e}"))?;
    if !mke2fs_status.success() {
        return Err("mke2fs failed".to_string());
    }

    eprintln!(
        "cargo-scarlet: wrote {} to {} ({}KB, source={}KB)",
        section_name,
        output_path.display(),
        size_kb,
        source_kb
    );
    Ok(())
}

fn file_size(path: &Path) -> Result<u64, String> {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|e| format!("failed to stat {}: {e}", path.display()))
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| format!("failed to create {}: {e}", dst.display()))?;
    for entry in
        fs::read_dir(src).map_err(|e| format!("failed to read_dir {}: {e}", src.display()))?
    {
        let entry = entry.map_err(|e| format!("failed to read dir entry: {e}"))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_symlink() {
            let link_target = fs::read_link(&src_path)
                .map_err(|e| format!("failed to read symlink {}: {e}", src_path.display()))?;
            remove_existing_path(&dst_path)?;
            std::os::unix::fs::symlink(&link_target, &dst_path).map_err(|e| {
                format!(
                    "failed to create symlink {} -> {}: {e}",
                    dst_path.display(),
                    link_target.display()
                )
            })?;
        } else if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path).map_err(|e| {
                format!(
                    "failed to copy {} -> {}: {e}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn remove_existing_path(path: &Path) -> Result<(), String> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path).map_err(|e| format!("failed to remove {}: {e}", path.display()))
    } else {
        fs::remove_file(path).map_err(|e| format!("failed to remove {}: {e}", path.display()))
    }
}

fn fetch_url_to_path(url: &str, dest: &Path) -> Result<(), String> {
    if let Some(rest) = url.strip_prefix("file://") {
        let source = Path::new(rest);
        fs::copy(source, dest).map_err(|e| {
            format!(
                "failed to copy file:// URL {url} to {}: {e}",
                dest.display()
            )
        })?;
        return Ok(());
    }

    let dest_str = dest
        .to_str()
        .ok_or_else(|| format!("destination path is not UTF-8: {}", dest.display()))?;
    let status = Command::new("curl")
        .args(["-fsSL", "-o", dest_str, url])
        .status()
        .map_err(|e| format!("failed to run curl: {e}"))?;
    if !status.success() {
        return Err(format!("curl failed to fetch {url}"));
    }
    Ok(())
}

fn fetch_url_cached(
    url: &str,
    cache_dir: &Path,
    expected_hash: Option<&str>,
) -> Result<(PathBuf, String), String> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    let hash = format!("{:016x}", hasher.finish());

    let url_path = url.split('?').next().unwrap_or(url);
    let basename = url_path.rsplit('/').next().unwrap_or("download");
    let cached_name = format!("{}-{}", &hash[..12], basename);
    let cached_path = cache_dir.join(&cached_name);

    if !cached_path.exists() {
        fs::create_dir_all(cache_dir)
            .map_err(|e| format!("failed to create cache dir {}: {e}", cache_dir.display()))?;

        eprintln!("cargo-scarlet: fetching {}", url);
        if let Err(e) = fetch_url_to_path(url, &cached_path) {
            let _ = fs::remove_file(&cached_path);
            return Err(e);
        }
    }

    let actual_hash = sha256_file(&cached_path)?;
    if let Some(expected) = expected_hash
        && actual_hash != expected
    {
        return Err(format!(
            "hash mismatch for {}: expected {}, got {}",
            url, expected, actual_hash
        ));
    }

    Ok((cached_path, actual_hash))
}

fn cached_url_path(url: &str, cache_dir: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    let hash = format!("{:016x}", hasher.finish());
    let url_path = url.split('?').next().unwrap_or(url);
    let basename = url_path.rsplit('/').next().unwrap_or("download");
    cache_dir.join(format!("{}-{}", &hash[..12], basename))
}

fn fetch_url_cached_with_offline(
    url: &str,
    cache_dir: &Path,
    expected_hash: Option<&str>,
    offline: bool,
) -> Result<(PathBuf, String), String> {
    let cached_path = cached_url_path(url, cache_dir);
    if offline && !cached_path.exists() {
        return Err(format!(
            "--offline: file {url} is not present in .scarlet/cache/files"
        ));
    }
    fetch_url_cached(url, cache_dir, expected_hash)
}

fn fetch_archive_by_sha256(
    url: &str,
    expected_sha256: &str,
    cache_dir: &Path,
    offline: bool,
) -> Result<PathBuf, String> {
    let expected_sha256 = normalize_sha256(expected_sha256)?;
    let hash = expected_sha256
        .strip_prefix("sha256:")
        .ok_or("normalized SHA-256 is missing sha256: prefix")?;
    let cached_path = cache_dir.join(hash);

    if cached_path.exists() {
        let actual_sha256 = sha256_file(&cached_path)?;
        if actual_sha256 != expected_sha256 {
            return Err(format!(
                "archive SHA-256 mismatch for {url}: expected {expected_sha256}, got {actual_sha256}"
            ));
        }
        return Ok(cached_path);
    }

    if offline {
        return Err(format!(
            "--offline: archive {expected_sha256} is not present in .scarlet/cache/files; run without --offline or run `cargo scarlet update`"
        ));
    }

    fs::create_dir_all(cache_dir).map_err(|error| {
        format!(
            "failed to create cache dir {}: {error}",
            cache_dir.display()
        )
    })?;
    let temporary_path = cache_dir.join(format!(".{hash}.tmp-{}", std::process::id()));

    if let Err(e) = fetch_url_to_path(url, &temporary_path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(e);
    }
    let actual_sha256 = sha256_file(&temporary_path)?;
    if actual_sha256 != expected_sha256 {
        let _ = fs::remove_file(&temporary_path);
        return Err(format!(
            "archive SHA-256 mismatch for {url}: expected {expected_sha256}, got {actual_sha256}"
        ));
    }

    fs::rename(&temporary_path, &cached_path).map_err(|error| {
        format!(
            "failed to move downloaded archive {} into {}: {error}",
            temporary_path.display(),
            cached_path.display()
        )
    })?;
    Ok(cached_path)
}

fn archive_entry_relative_path(
    path: &Path,
    strip_components: usize,
) -> Result<Option<PathBuf>, String> {
    if path.is_absolute() {
        return Err(format!("refusing absolute archive path {}", path.display()));
    }

    let mut clean = PathBuf::new();
    for (index, component) in path.components().skip(strip_components).enumerate() {
        match component {
            Component::Normal(component) => clean.push(component),
            Component::CurDir if index == 0 => {}
            Component::CurDir => {
                return Err(format!(
                    "refusing archive path {} with embedded current-directory component",
                    path.display()
                ));
            }
            Component::ParentDir => {
                return Err(format!("refusing archive path {} with ..", path.display()));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("refusing absolute archive path {}", path.display()));
            }
        }
    }

    if clean.as_os_str().is_empty() {
        Ok(None)
    } else {
        Ok(Some(clean))
    }
}

fn lexical_absolute_components(path: &Path) -> Option<Vec<std::ffi::OsString>> {
    if !path.is_absolute() {
        return None;
    }

    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) => return None,
            Component::RootDir | Component::CurDir => {}
            Component::Normal(component) => components.push(component.to_os_string()),
            Component::ParentDir => {
                components.pop()?;
            }
        }
    }
    Some(components)
}

fn lexical_contains(root: &Path, target: &Path) -> bool {
    let Some(root_components) = lexical_absolute_components(root) else {
        return false;
    };
    let Some(target_components) = lexical_absolute_components(target) else {
        return false;
    };
    target_components.starts_with(&root_components)
}

fn ensure_no_symlink_ancestors(dest_root: &Path, dest: &Path) -> Result<(), String> {
    if !lexical_contains(dest_root, dest) {
        return Err(format!(
            "refusing archive path {} (escapes extraction root)",
            dest.display()
        ));
    }

    let relative = dest.strip_prefix(dest_root).map_err(|error| {
        format!(
            "failed to inspect archive destination {} below {}: {error}",
            dest.display(),
            dest_root.display()
        )
    })?;
    let mut current = dest_root.to_path_buf();
    check_not_symlink(&current)?;
    for component in relative.components() {
        current.push(component.as_os_str());
        check_not_symlink(&current)?;
    }
    Ok(())
}

fn check_not_symlink(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "refusing archive entry below symlink {}",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("failed to inspect {}: {error}", path.display())),
    }
}

fn archive_symlink_target_path(
    dest_root: &Path,
    dest: &Path,
    target: &Path,
) -> Result<PathBuf, String> {
    if target
        .components()
        .any(|component| matches!(component, Component::Prefix(_)))
    {
        return Err(format!(
            "refusing symlink {} (escapes extraction root)",
            target.display()
        ));
    }

    if target.is_absolute() {
        let mut resolved = dest_root.to_path_buf();
        for component in target.components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(component) => resolved.push(component),
                Component::ParentDir => resolved.push(".."),
                Component::Prefix(_) => unreachable!(),
            }
        }
        Ok(resolved)
    } else {
        let parent = dest.parent().ok_or_else(|| {
            format!(
                "failed to determine parent directory for archive path {}",
                dest.display()
            )
        })?;
        Ok(parent.join(target))
    }
}

fn extract_archive_entries<R: Read>(
    archive: &mut tar::Archive<R>,
    strip_components: usize,
    dest_root: &Path,
) -> Result<(), String> {
    for entry in archive
        .entries()
        .map_err(|error| format!("failed to read archive entries: {error}"))?
    {
        let mut entry = entry.map_err(|error| format!("failed to read archive entry: {error}"))?;
        let path = entry
            .path()
            .map_err(|error| format!("failed to read archive entry path: {error}"))?
            .into_owned();
        if entry.path_bytes().starts_with(b"/") {
            return Err(format!("refusing absolute archive path {}", path.display()));
        }
        let Some(relative) = archive_entry_relative_path(&path, strip_components)? else {
            continue;
        };
        let dest = dest_root.join(relative);
        if !dest.starts_with(dest_root) || !lexical_contains(dest_root, &dest) {
            return Err(format!(
                "refusing archive path {} (escapes extraction root)",
                path.display()
            ));
        }

        let entry_type = entry.header().entry_type();
        if entry_type.is_hard_link() {
            return Err(format!(
                "refusing hardlink archive entry {}",
                path.display()
            ));
        }
        if !entry_type.is_file() && !entry_type.is_dir() && !entry_type.is_symlink() {
            return Err(format!("refusing special archive entry {}", path.display()));
        }

        if entry_type.is_file() {
            let parent = dest.parent().ok_or_else(|| {
                format!(
                    "failed to determine parent directory for archive path {}",
                    path.display()
                )
            })?;
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
            ensure_no_symlink_ancestors(dest_root, &dest)?;
            let mut file = fs::File::create(&dest)
                .map_err(|error| format!("failed to create {}: {error}", dest.display()))?;
            std::io::copy(&mut entry, &mut file)
                .map_err(|error| format!("failed to extract {}: {error}", path.display()))?;
            let mode =
                entry.header().mode().map_err(|error| {
                    format!("failed to read mode for {}: {error}", path.display())
                })? & (0o7777 & !0o7000);
            fs::set_permissions(&dest, std::os::unix::fs::PermissionsExt::from_mode(mode))
                .map_err(|error| {
                    format!("failed to set permissions on {}: {error}", dest.display())
                })?;
        } else if entry_type.is_dir() {
            ensure_no_symlink_ancestors(dest_root, &dest)?;
            fs::create_dir_all(&dest)
                .map_err(|error| format!("failed to create {}: {error}", dest.display()))?;
            ensure_no_symlink_ancestors(dest_root, &dest)?;
            let mode =
                entry.header().mode().map_err(|error| {
                    format!("failed to read mode for {}: {error}", path.display())
                })? & (0o7777 & !0o7000);
            fs::set_permissions(&dest, std::os::unix::fs::PermissionsExt::from_mode(mode))
                .map_err(|error| {
                    format!("failed to set permissions on {}: {error}", dest.display())
                })?;
        } else {
            let target = entry
                .link_name()
                .map_err(|error| {
                    format!(
                        "failed to read symlink target for {}: {error}",
                        path.display()
                    )
                })?
                .ok_or_else(|| format!("archive symlink {} has no target", path.display()))?;
            let resolved_target = archive_symlink_target_path(dest_root, &dest, target.as_ref())?;
            if !lexical_contains(dest_root, &resolved_target) {
                return Err(format!(
                    "refusing symlink {} (escapes extraction root)",
                    target.display()
                ));
            }
            let parent = dest.parent().ok_or_else(|| {
                format!(
                    "failed to determine parent directory for archive path {}",
                    path.display()
                )
            })?;
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
            ensure_no_symlink_ancestors(dest_root, &dest)?;
            std::os::unix::fs::symlink(target.as_ref(), &dest).map_err(|error| {
                format!(
                    "failed to create symlink {} -> {}: {error}",
                    dest.display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

fn extract_archive_safe(
    archive_path: &Path,
    format: ArchiveFormat,
    strip_components: usize,
    dest_root: &Path,
) -> Result<(), String> {
    let dest_root = if dest_root.is_absolute() {
        dest_root.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("failed to get current directory: {error}"))?
            .join(dest_root)
    };
    fs::create_dir_all(&dest_root)
        .map_err(|error| format!("failed to create {}: {error}", dest_root.display()))?;
    ensure_no_symlink_ancestors(&dest_root, &dest_root)?;

    let file = fs::File::open(archive_path)
        .map_err(|error| format!("failed to open archive {}: {error}", archive_path.display()))?;
    match format {
        ArchiveFormat::Tar => {
            let mut archive = tar::Archive::new(file);
            extract_archive_entries(&mut archive, strip_components, &dest_root)
        }
        ArchiveFormat::TarGz => {
            let decoder = flate2::read::GzDecoder::new(file);
            let mut archive = tar::Archive::new(decoder);
            extract_archive_entries(&mut archive, strip_components, &dest_root)
        }
        ArchiveFormat::TarZst => {
            let decoder = zstd::stream::Decoder::new(file)
                .map_err(|error| format!("failed to open zstd archive: {error}"))?;
            let mut archive = tar::Archive::new(decoder);
            extract_archive_entries(&mut archive, strip_components, &dest_root)
        }
        ArchiveFormat::TarXz => {
            let decoder = xz2::read::XzDecoder::new(file);
            let mut archive = tar::Archive::new(decoder);
            extract_archive_entries(&mut archive, strip_components, &dest_root)
        }
    }
}

fn archive_destination(staging_dir: &Path, to: &str) -> Result<PathBuf, String> {
    let relative = Path::new(to.trim_start_matches('/'));
    let Some(relative) = archive_entry_relative_path(relative, 0)? else {
        return Ok(staging_dir.to_path_buf());
    };
    Ok(staging_dir.join(relative))
}

fn apply_archive_layer(
    archive: &ResolvedArchive,
    staging_dir: &Path,
    cache_dir: &Path,
    offline: bool,
    layer_locks: &mut Vec<LayerLock>,
) -> Result<(), String> {
    let archive_path = fetch_archive_by_sha256(&archive.url, &archive.sha256, cache_dir, offline)?;
    let dest_root = archive_destination(staging_dir, &archive.to)?;
    extract_archive_safe(
        &archive_path,
        archive.format.clone(),
        archive.strip_components,
        &dest_root,
    )?;
    layer_locks.push(LayerLock::Archive {
        source: LockPackageSource::archive(archive.url.clone(), archive.sha256.clone()),
        to: archive.to.clone(),
        format: archive.format.clone(),
        strip_components: archive.strip_components,
        hash: archive.sha256.clone(),
    });
    Ok(())
}

fn apply_copy_layer(
    file: &ResolvedFile,
    staging_dir: &Path,
    cache_dir: &Path,
    prev_section_lock: Option<&SectionLock>,
    tpl_ctx: &TemplateContext,
    offline: bool,
    layer_locks: &mut Vec<LayerLock>,
) -> Result<(), String> {
    let local_path = match &file.source {
        FileSource::Local(p) => p.clone(),
        FileSource::Url(u) => {
            let expected = prev_section_lock.and_then(|s| {
                s.copy_locks()
                    .find(|lock| copy_lock_matches_input(lock, file))
                    .map(|lock| lock.hash)
            });
            let (path, hash) =
                fetch_url_cached_with_offline(u, cache_dir, expected.as_deref(), offline)?;
            layer_locks.push(LayerLock::Copy {
                source: u.clone(),
                to: file.to.clone(),
                template: file.template,
                hash,
            });
            path
        }
    };

    let dest = staging_dir.join(file.to.trim_start_matches('/'));
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    if local_path.is_dir() {
        copy_dir_recursive(&local_path, &dest)?;
    } else if file.template {
        let content = fs::read_to_string(&local_path)
            .map_err(|e| format!("failed to read template {}: {e}", local_path.display()))?;
        let expanded = tpl_ctx.expand(&content);
        fs::write(&dest, expanded).map_err(|e| {
            format!(
                "failed to write template {} -> {}: {e}",
                local_path.display(),
                dest.display()
            )
        })?;
    } else {
        copy_path_or_dir(&local_path, &dest)?;
    }

    Ok(())
}

fn image_output_path(
    project: &Path,
    images: &BTreeMap<String, ManifestImageSection>,
    image_name: &str,
) -> Result<PathBuf, String> {
    let image = images
        .get(image_name)
        .ok_or_else(|| format!("unknown image '{}'", image_name))?;
    let output = image.output.as_deref().unwrap_or(".scarlet/images/output");
    Ok(project.join(output))
}

fn initramfs_path_from_layers(
    project: &Path,
    images: &BTreeMap<String, ManifestImageSection>,
    section: &ResolvedSection,
) -> Option<PathBuf> {
    section.layers.iter().find_map(|layer| match layer {
        ResolvedLayer::Copy(file) if file.to == "/boot/initramfs" => match &file.source {
            FileSource::Local(path) => Some(path.clone()),
            FileSource::Url(_) => None,
        },
        ResolvedLayer::Image { source, to } if to == "/boot/initramfs" => {
            image_output_path(project, images, source).ok()
        }
        _ => None,
    })
}

fn plugin_packages_from_layers(
    project: &Path,
    images: &BTreeMap<String, ManifestImageSection>,
    section: &ResolvedSection,
) -> Result<Vec<PluginRequestPackage>, String> {
    let mut packages = Vec::new();
    for layer in &section.layers {
        match layer {
            ResolvedLayer::Copy(file) => {
                if file.to != "/boot/initramfs"
                    && let FileSource::Local(source) = &file.source
                {
                    packages.push(PluginRequestPackage {
                        source: source.display().to_string(),
                        to: file.to.clone(),
                    });
                }
            }
            ResolvedLayer::Image { source, to } => {
                packages.push(PluginRequestPackage {
                    source: image_output_path(project, images, source)?
                        .display()
                        .to_string(),
                    to: to.clone(),
                });
            }
            ResolvedLayer::Archive(_) | ResolvedLayer::Package(_) => {}
        }
    }
    Ok(packages)
}

fn copy_path_or_dir(source: &Path, dest: &Path) -> Result<(), String> {
    if source.is_dir() {
        copy_dir_recursive(source, dest)
    } else {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
        }
        fs::copy(source, dest).map_err(|e| {
            format!(
                "failed to copy {} -> {}: {e}",
                source.display(),
                dest.display()
            )
        })?;
        Ok(())
    }
}

fn install_package(
    staging_dir: &Path,
    pkg: &ResolvedPackage,
    project: &Path,
    target_triple: &str,
    profile: &str,
    prev_lock: Option<&PackageLock>,
) -> Result<Option<PackageLock>, String> {
    let dest = staging_dir.join(pkg.to.trim_start_matches('/'));

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }

    match pkg.kind.as_deref() {
        Some("cargo") => {
            let source = pkg
                .local_source
                .as_ref()
                .ok_or("cargo package missing source")?;
            let package_name = pkg.package_name.as_deref().unwrap_or("user-bin");
            let bin_name = pkg.bin.as_deref().unwrap_or(package_name);

            let userspace_triple = userspace_target_triple(target_triple);
            let profile_dir = if profile == "release" {
                "release"
            } else {
                "debug"
            };
            let target_dir = project_cargo_target_dir(project);

            let binary = {
                eprintln!(
                    "cargo-scarlet: building {} ({}) for {}...",
                    package_name, bin_name, userspace_triple
                );
                let mut cmd = Command::new("cargo");
                cmd.arg("build");
                if profile == "release" {
                    cmd.arg("--release");
                }
                cmd.arg("--manifest-path")
                    .arg(source.join("Cargo.toml"))
                    .arg("--target")
                    .arg(&userspace_triple);
                cmd.env("CARGO_TARGET_DIR", &target_dir);

                if let Some(bin) = pkg.bin.as_deref() {
                    cmd.arg("--bin").arg(bin);
                }
                if pkg.default_features == Some(false) {
                    cmd.arg("--no-default-features");
                }
                if !pkg.features.is_empty() {
                    cmd.arg("--features").arg(pkg.features.join(","));
                }

                let status = cmd
                    .current_dir(source)
                    .status()
                    .map_err(|e| format!("failed to run cargo build: {e}"))?;

                if !status.success() {
                    return Err(format!(
                        "cargo build failed for {} (bin {})",
                        package_name, bin_name
                    ));
                }

                let built = target_dir
                    .join(&userspace_triple)
                    .join(profile_dir)
                    .join(bin_name);
                if !built.exists() {
                    return Err(format!(
                        "cargo build succeeded but binary not found: {}",
                        built.display()
                    ));
                }
                built
            };
            fs::copy(&binary, &dest)
                .map_err(|e| format!("failed to copy {}: {e}", binary.display()))?;

            let hash = sha256_file(&binary)?;
            let (source, git_url, resolved_rev) = match &pkg.source {
                Some(PackageSource::Git { git, .. }) => {
                    let resolved_rev = pkg.resolved_rev.clone();
                    let source = resolved_rev
                        .as_ref()
                        .map(|rev| LockPackageSource::git(git.clone(), rev.clone()));
                    (source, Some(git.clone()), resolved_rev)
                }
                _ => (package_lock_source(project, pkg)?, None, None),
            };
            Ok(Some(PackageLock {
                kind: "cargo".to_string(),
                source,
                git: git_url,
                git_ref: package_git_ref(pkg),
                resolved_rev,
                package: pkg.package_name.clone(),
                bin: pkg.bin.clone(),
                features: pkg.features.clone(),
                default_features: pkg.default_features,
                to: pkg.to.clone(),
                output: None,
                hash,
            }))
        }
        Some("script") => {
            let source = pkg
                .local_source
                .as_ref()
                .ok_or("script package missing source")?;
            let script_path = if source.is_absolute() {
                source.clone()
            } else {
                project.join(source)
            };
            if !script_path.exists() {
                return Err(format!("script not found: {}", script_path.display()));
            }

            let (script_output, copy_dest) = match &pkg.output {
                Some(output) => {
                    if let Some(parent) = output.parent() {
                        fs::create_dir_all(parent)
                            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
                    }
                    (output.clone(), dest.clone())
                }
                None => {
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent)
                            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
                    }
                    (dest.clone(), PathBuf::new())
                }
            };

            if let (Some(output_path), Some(prev)) = (&pkg.output, prev_lock) {
                if output_path.exists() {
                    let current_hash = sha256_file(output_path)?;
                    if current_hash == prev.hash {
                        eprintln!(
                            "cargo-scarlet: script output {} unchanged, skipping",
                            output_path.display()
                        );
                    } else {
                        let status = Command::new("sh")
                            .arg(&script_path)
                            .arg(&script_output)
                            .current_dir(project)
                            .status()
                            .map_err(|e| {
                                format!("failed to run script {}: {e}", script_path.display())
                            })?;
                        if !status.success() {
                            return Err(format!("script failed: {}", script_path.display()));
                        }
                    }
                } else {
                    let status = Command::new("sh")
                        .arg(&script_path)
                        .arg(&script_output)
                        .current_dir(project)
                        .status()
                        .map_err(|e| {
                            format!("failed to run script {}: {e}", script_path.display())
                        })?;
                    if !status.success() {
                        return Err(format!("script failed: {}", script_path.display()));
                    }
                }
            } else {
                let status = Command::new("sh")
                    .arg(&script_path)
                    .arg(&script_output)
                    .current_dir(project)
                    .status()
                    .map_err(|e| format!("failed to run script {}: {e}", script_path.display()))?;
                if !status.success() {
                    return Err(format!("script failed: {}", script_path.display()));
                }
            }

            if !copy_dest.as_os_str().is_empty() {
                if script_output.is_dir() {
                    copy_dir_recursive(&script_output, &copy_dest)?;
                } else if script_output.exists() {
                    if let Some(parent) = copy_dest.parent() {
                        fs::create_dir_all(parent)
                            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
                    }
                    fs::copy(&script_output, &copy_dest).map_err(|e| {
                        format!(
                            "failed to copy {} -> {}: {e}",
                            script_output.display(),
                            copy_dest.display()
                        )
                    })?;
                }
            }

            let hash = if script_output.is_dir() {
                sha256_dir(&script_output)?
            } else if script_output.exists() {
                sha256_file(&script_output)?
            } else {
                "missing".to_string()
            };
            Ok(Some(PackageLock {
                kind: "script".to_string(),
                source: package_lock_source(project, pkg)?,
                git: None,
                git_ref: None,
                resolved_rev: None,
                package: None,
                bin: None,
                features: Vec::new(),
                default_features: None,
                to: pkg.to.clone(),
                output: package_output_lock_path(project, pkg)?,
                hash,
            }))
        }
        _ => {
            let from = pkg.from.as_ref().ok_or_else(|| {
                format!(
                    "package (to={}) missing 'from' path and has unknown kind {:?}",
                    pkg.to, pkg.kind
                )
            })?;
            if from.is_dir() {
                copy_dir_contents(from, &dest, &[])?;
            } else if from.exists() {
                fs::copy(from, &dest)
                    .map_err(|e| format!("failed to copy {}: {e}", from.display()))?;
            } else {
                eprintln!(
                    "cargo-scarlet: warning: source not found: {}",
                    from.display()
                );
            }
            Ok(None)
        }
    }
}

fn build_initramfs_newc_from_staging(staging_dir: &Path, output_path: &Path) -> Result<(), String> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }

    let mut output_file = fs::File::create(output_path)
        .map_err(|e| format!("failed to create {}: {e}", output_path.display()))?;

    write_newc_tree(&mut output_file, staging_dir, staging_dir)?;
    write_newc_trailer(&mut output_file)?;

    Ok(())
}

fn write_newc_trailer(output: &mut fs::File) -> Result<(), String> {
    use std::io::Write;
    let trailer_name = "TRAILER!!!";
    let name_size = trailer_name.len() + 1;
    let header = format!(
        "070701{ino:08x}{mode:08x}{uid:08x}{gid:08x}{nlink:08x}{mtime:08x}{file_size:08x}{dev_major:08x}{dev_minor:08x}{rdev_major:08x}{rdev_minor:08x}{name_size:08x}{check:08x}",
        ino = 0,
        mode = 0,
        uid = 0,
        gid = 0,
        nlink = 1,
        mtime = 0,
        file_size = 0,
        dev_major = 0,
        dev_minor = 0,
        rdev_major = 0,
        rdev_minor = 0,
        name_size = name_size,
        check = 0,
    );
    output
        .write_all(header.as_bytes())
        .map_err(|e| format!("write failed: {e}"))?;
    output
        .write_all(trailer_name.as_bytes())
        .map_err(|e| format!("write failed: {e}"))?;
    output
        .write_all(&[0])
        .map_err(|e| format!("write failed: {e}"))?;
    pad4(output, 110 + name_size)?;
    Ok(())
}

fn normalized_args() -> Vec<String> {
    let mut args = std::env::args().collect::<Vec<_>>();
    if args.get(1).is_some_and(|arg| arg == "scarlet") {
        args.remove(1);
    }
    args
}

fn normalize_project_path(path: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(path).map_err(|error| format!("failed to resolve {}: {error}", path.display()))
}

fn render_kernel_features(features: &[String]) -> String {
    features
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_dependency_spec(project_root: &Path, module: &ModuleConfig) -> Result<String, String> {
    let mut parts = Vec::new();

    if let Some(version) = &module.version {
        parts.push(format!("version = \"{version}\""));
        if let Some(registry) = &module.registry {
            parts.push(format!("registry = \"{registry}\""));
        }
    }

    if let Some(path) = &module.path {
        let absolute = project_root.join(path);
        let generated_root = project_root.join(".scarlet/scarlet-modules");
        let relative = pathdiff(&absolute, &generated_root)?;
        parts.push(format!("path = \"{}\"", relative.display()));
    }

    if let Some(git) = &module.git {
        parts.push(format!("git = \"{git}\""));
    }
    if let Some(rev) = &module.rev {
        parts.push(format!("rev = \"{rev}\""));
    }
    if let Some(branch) = &module.branch {
        parts.push(format!("branch = \"{branch}\""));
    }
    if let Some(tag) = &module.tag {
        parts.push(format!("tag = \"{tag}\""));
    }
    if let Some(package) = &module.package {
        parts.push(format!("package = \"{package}\""));
    }
    if let Some(features) = &module.features {
        let rendered = features
            .iter()
            .map(|feature| format!("\"{feature}\""))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!("features = [{rendered}]"));
    }
    if let Some(default_features) = module.default_features {
        parts.push(format!("default-features = {default_features}"));
    }

    Ok(parts.join(", "))
}

fn write_if_changed(path: &Path, contents: &str) -> Result<(), String> {
    if let Ok(existing) = fs::read_to_string(path)
        && existing == contents
    {
        return Ok(());
    }

    fs::write(path, contents)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoMetadataPackage>,
    resolve: Option<CargoResolve>,
}

#[derive(Debug, Deserialize)]
struct CargoMetadataPackage {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct CargoResolve {
    nodes: Vec<CargoResolveNode>,
}

#[derive(Debug, Deserialize)]
struct CargoResolveNode {
    id: String,
    features: Vec<String>,
}

fn metadata_check(
    project: &Path,
    target: &str,
    kernel_package: &str,
    disabled_kernel_features: &[String],
) -> Result<(), String> {
    let mut command = Command::new("cargo");
    command
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--filter-platform")
        .arg(target)
        .current_dir(project);

    eprintln!(
        "cargo-scarlet: running in {} -> cargo {}",
        project.display(),
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    );

    let output = command
        .output()
        .map_err(|error| format!("failed to run cargo metadata: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "cargo metadata failed with status {}: {stderr}",
            output.status
        ));
    }

    let metadata: CargoMetadata = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("failed to parse cargo metadata output: {error}"))?;
    ensure_disabled_kernel_features(&metadata, kernel_package, disabled_kernel_features)
}

fn ensure_disabled_kernel_features(
    metadata: &CargoMetadata,
    kernel_package: &str,
    disabled_kernel_features: &[String],
) -> Result<(), String> {
    if disabled_kernel_features.is_empty() {
        return Ok(());
    }

    let package_ids = metadata
        .packages
        .iter()
        .filter(|package| package.name == kernel_package)
        .map(|package| package.id.as_str())
        .collect::<Vec<_>>();
    if package_ids.is_empty() {
        return Err(format!(
            "kernel package `{kernel_package}` was not found in cargo metadata"
        ));
    }

    let resolve = metadata
        .resolve
        .as_ref()
        .ok_or("cargo metadata did not include a dependency resolve graph")?;
    let mut conflicts = resolve
        .nodes
        .iter()
        .filter(|node| package_ids.contains(&node.id.as_str()))
        .flat_map(|node| node.features.iter())
        .filter(|feature| disabled_kernel_features.contains(feature))
        .cloned()
        .collect::<Vec<_>>();
    conflicts.sort();
    conflicts.dedup();

    if conflicts.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "kernel feature(s) explicitly disabled in scarlet.toml were enabled by Cargo feature unification: {}",
            conflicts.join(", ")
        ))
    }
}

fn cargo_key_to_rust_identifier(name: &str) -> String {
    name.replace('-', "_")
}

fn pathdiff(path: &Path, base: &Path) -> Result<PathBuf, String> {
    let normalized_path = normalize_path_lexically(path);
    let normalized_base = normalize_path_lexically(base);
    let path_components = normalized_path.components().collect::<Vec<_>>();
    let base_components = normalized_base.components().collect::<Vec<_>>();

    let mut common = 0usize;
    while common < path_components.len()
        && common < base_components.len()
        && path_components[common] == base_components[common]
    {
        common += 1;
    }

    let mut result = PathBuf::new();
    for _ in common..base_components.len() {
        result.push("..");
    }
    for component in &path_components[common..] {
        result.push(component.as_os_str());
    }

    if result.as_os_str().is_empty() {
        result.push(".");
    }

    Ok(result)
}

fn package_lock_source(
    project: &Path,
    pkg: &ResolvedPackage,
) -> Result<Option<LockPackageSource>, String> {
    match &pkg.source {
        Some(PackageSource::Path(_) | PackageSource::PathTable { .. }) => {
            let source = pkg
                .local_source
                .as_ref()
                .ok_or("path package missing local source")?;
            let relative = pathdiff(source, project)?;
            Ok(Some(LockPackageSource::path(
                relative.to_string_lossy().to_string(),
            )))
        }
        Some(PackageSource::Git { git, .. }) => match &pkg.resolved_rev {
            Some(rev) => Ok(Some(LockPackageSource::git(git.clone(), rev.clone()))),
            None => Ok(None),
        },
        None => Ok(None),
    }
}

fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }

    if normalized.as_os_str().is_empty() {
        normalized.push(".");
    }

    normalized
}

fn absolutize_from_current_dir(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .map_err(|error| format!("failed to get current directory: {error}"))?
            .join(path))
    }
}

fn copy_dir_contents(
    source: &Path,
    destination: &Path,
    skip_suffixes: &[String],
) -> Result<(), String> {
    for entry in sorted_dir_entries(source)? {
        let file_name = entry
            .file_name()
            .into_string()
            .map_err(|_| format!("non-UTF-8 file name under {}", source.display()))?;
        let source_path = entry.path();
        let destination_path = destination.join(file_name);
        if source_path.is_dir() {
            fs::create_dir_all(&destination_path).map_err(|error| {
                format!("failed to create {}: {error}", destination_path.display())
            })?;
            copy_permissions(&source_path, &destination_path)?;
            copy_dir_contents(&source_path, &destination_path, skip_suffixes)?;
        } else if source_path.is_file() && !should_skip_path(&source_path, skip_suffixes) {
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
            }
            fs::copy(&source_path, &destination_path).map_err(|error| {
                format!(
                    "failed to copy {} to {}: {error}",
                    source_path.display(),
                    destination_path.display()
                )
            })?;
            copy_permissions(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

fn should_skip_path(path: &Path, skip_suffixes: &[String]) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    skip_suffixes.iter().any(|suffix| name.ends_with(suffix))
}

fn copy_permissions(source: &Path, destination: &Path) -> Result<(), String> {
    let permissions = fs::metadata(source)
        .map_err(|error| format!("failed to stat {}: {error}", source.display()))?
        .permissions();
    fs::set_permissions(destination, permissions)
        .map_err(|error| format!("failed to chmod {}: {error}", destination.display()))
}

fn sorted_dir_entries(path: &Path) -> Result<Vec<fs::DirEntry>, String> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("failed to read dir entry in {}: {error}", path.display()))?;
    entries.sort_by_key(|entry| entry.path());
    Ok(entries)
}

fn write_newc_tree(output: &mut fs::File, root: &Path, path: &Path) -> Result<(), String> {
    for entry in sorted_dir_entries(path)? {
        let entry_path = entry.path();
        let relative = entry_path
            .strip_prefix(root)
            .map_err(|error| format!("failed to strip path prefix: {error}"))?;
        let name = relative
            .to_str()
            .ok_or_else(|| format!("non-UTF-8 archive path: {}", relative.display()))?;
        let metadata = fs::symlink_metadata(&entry_path)
            .map_err(|error| format!("failed to stat {}: {error}", entry_path.display()))?;

        if metadata.is_dir() {
            write_newc_entry(output, name, 0o040000 | unix_mode(&metadata), &[])?;
            write_newc_tree(output, root, &entry_path)?;
        } else if metadata.file_type().is_symlink() {
            let target = fs::read_link(&entry_path).map_err(|error| {
                format!("failed to read symlink {}: {error}", entry_path.display())
            })?;
            let target = target
                .to_str()
                .ok_or_else(|| format!("non-UTF-8 symlink target: {}", target.display()))?;
            write_newc_entry(
                output,
                name,
                0o120000 | unix_mode(&metadata),
                target.as_bytes(),
            )?;
        } else if metadata.is_file() {
            let mut contents = Vec::new();
            fs::File::open(&entry_path)
                .map_err(|error| format!("failed to open {}: {error}", entry_path.display()))?
                .read_to_end(&mut contents)
                .map_err(|error| format!("failed to read {}: {error}", entry_path.display()))?;
            write_newc_entry(output, name, 0o100000 | unix_mode(&metadata), &contents)?;
        }
    }
    Ok(())
}

fn write_newc_entry(
    output: &mut fs::File,
    name: &str,
    mode: u32,
    contents: &[u8],
) -> Result<(), String> {
    let name_size = name.len() + 1;
    let file_size = contents.len();
    let header = format!(
        "070701{ino:08x}{mode:08x}{uid:08x}{gid:08x}{nlink:08x}{mtime:08x}{file_size:08x}{dev_major:08x}{dev_minor:08x}{rdev_major:08x}{rdev_minor:08x}{name_size:08x}{check:08x}",
        ino = 0,
        mode = mode,
        uid = 0,
        gid = 0,
        nlink = 1,
        mtime = 0,
        file_size = file_size,
        dev_major = 0,
        dev_minor = 0,
        rdev_major = 0,
        rdev_minor = 0,
        name_size = name_size,
        check = 0,
    );
    output
        .write_all(header.as_bytes())
        .and_then(|_| output.write_all(name.as_bytes()))
        .and_then(|_| output.write_all(&[0]))
        .map_err(|error| format!("failed to write cpio header: {error}"))?;
    pad4(output, 110 + name_size)?;
    output
        .write_all(contents)
        .map_err(|error| format!("failed to write cpio contents: {error}"))?;
    pad4(output, file_size)?;
    Ok(())
}

fn pad4(output: &mut fs::File, size: usize) -> Result<(), String> {
    let padding = (4 - (size % 4)) % 4;
    if padding != 0 {
        output
            .write_all(&[0; 3][..padding])
            .map_err(|error| format!("failed to write cpio padding: {error}"))?;
    }
    Ok(())
}

#[cfg(unix)]
fn unix_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn unix_mode(metadata: &fs::Metadata) -> u32 {
    if metadata.permissions().readonly() {
        0o444
    } else {
        0o755
    }
}

fn build_loadable_module(
    module_path: &Path,
    target: Option<&str>,
    output: Option<&Path>,
    release: bool,
) -> Result<(), String> {
    let target = target.ok_or("--target is required when using --module")?;
    let module_dir = fs::canonicalize(module_path).map_err(|e| {
        format!(
            "failed to resolve module path {}: {e}",
            module_path.display()
        )
    })?;

    let module_name = read_module_toml_name(&module_dir).ok_or_else(|| {
        format!(
            "failed to read module name from module.toml in {}",
            module_dir.display()
        )
    })?;
    let package_name = read_cargo_package_name(&module_dir);

    let target_path = if Path::new(target).is_absolute() {
        PathBuf::from(target)
    } else {
        std::env::current_dir()
            .map_err(|e| format!("failed to get current directory: {e}"))?
            .join(target)
    };
    let target_path = fs::canonicalize(&target_path).map_err(|e| {
        format!(
            "failed to resolve target path {}: {e}",
            target_path.display()
        )
    })?;

    let target_triple = target_path
        .file_stem()
        .ok_or("target path has no file stem")?
        .to_string_lossy()
        .to_string();

    eprintln!(
        "cargo-scarlet: building loadable module {} (target: {})",
        module_dir.display(),
        target_path.display()
    );

    let mut command = Command::new("cargo");
    command.arg("rustc").arg("--target").arg(&target_path);
    if release {
        command.arg("--release");
    }
    command.arg("--").arg("--emit=obj").current_dir(&module_dir);

    let status = command
        .status()
        .map_err(|e| format!("failed to run cargo rustc: {e}"))?;

    if !status.success() {
        return Err(format!("cargo rustc failed with status {status}"));
    }

    let profile = if release { "release" } else { "debug" };
    let output_dir = module_dir.join("target").join(&target_triple).join(profile);
    let deps_dir = output_dir.join("deps");
    let lsm_filename = format!("{}.lsm", module_name);

    let mut object_files: Vec<std::path::PathBuf> = Vec::new();
    for entry in fs::read_dir(&deps_dir)
        .map_err(|e| format!("failed to read {}: {e}", deps_dir.display()))?
    {
        let entry = entry.map_err(|e| format!("failed to read dir entry: {e}"))?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "o") {
            object_files.push(path);
        }
    }

    let selected_object = if object_files.is_empty() {
        None
    } else if object_files.len() == 1 {
        Some(object_files.remove(0))
    } else {
        let mut normalized_names = vec![cargo_key_to_rust_identifier(&module_name)];
        if let Some(package_name) = package_name.as_deref() {
            let normalized_package_name = cargo_key_to_rust_identifier(package_name);
            if !normalized_names.contains(&normalized_package_name) {
                normalized_names.push(normalized_package_name);
            }
        }
        let candidates: Vec<_> = object_files
            .into_iter()
            .filter(|path| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|stem| normalized_names.iter().any(|name| stem.starts_with(name)))
                    .unwrap_or(false)
            })
            .collect();

        match candidates.len() {
            0 => {
                return Err(format!(
                    "multiple .o files in {}, but none match module name '{}'",
                    deps_dir.display(),
                    module_name
                ));
            }
            1 => Some(candidates.into_iter().next().unwrap()),
            _ => {
                return Err(format!(
                    "multiple .o files in {} match module name '{}'; cannot determine which to use",
                    deps_dir.display(),
                    module_name
                ));
            }
        }
    };

    let mut built = false;
    if let Some(object_path) = selected_object {
        let lsm_path = output_dir.join(&lsm_filename);
        fs::rename(&object_path, &lsm_path)
            .map_err(|e| format!("failed to rename object file to .lsm: {e}"))?;
        eprintln!("cargo-scarlet: produced {}", lsm_path.display());
        built = true;
    }

    if !built {
        for entry in fs::read_dir(&output_dir)
            .map_err(|e| format!("failed to read {}: {e}", output_dir.display()))?
        {
            let entry = entry.map_err(|e| format!("failed to read dir entry: {e}"))?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "lsm") {
                built = true;
                break;
            }
        }
    }

    if !built {
        return Err("no .o files produced by cargo rustc".to_string());
    }

    if let Some(output) = output {
        let output_dir = std::env::current_dir()
            .map_err(|e| format!("failed to get current directory: {e}"))?
            .join(output);
        fs::create_dir_all(&output_dir).map_err(|e| format!("failed to create output dir: {e}"))?;
        let lsm_path = module_dir
            .join("target")
            .join(&target_triple)
            .join(profile)
            .join(&lsm_filename);
        let dest = output_dir.join(&lsm_filename);
        fs::copy(&lsm_path, &dest).map_err(|e| format!("failed to copy .lsm to output: {e}"))?;
        eprintln!("cargo-scarlet: copied to {}", dest.display());
    }

    Ok(())
}

fn read_module_toml_name(module_dir: &Path) -> Option<String> {
    let content = fs::read_to_string(module_dir.join("module.toml")).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("name")
            && let Some(eq_pos) = trimmed.find('=')
            && let Some(value) = trimmed.get(eq_pos + 1..).map(str::trim)
            && value.starts_with('"')
            && value.ends_with('"')
            && value.len() >= 2
        {
            return Some(value[1..value.len() - 1].to_string());
        }
    }
    None
}

fn read_cargo_package_name(module_dir: &Path) -> Option<String> {
    let content = fs::read_to_string(module_dir.join("Cargo.toml")).ok()?;
    let mut in_package_section = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package_section = trimmed == "[package]";
            continue;
        }

        if in_package_section
            && trimmed.starts_with("name")
            && let Some(eq_pos) = trimmed.find('=')
            && let Some(value) = trimmed.get(eq_pos + 1..).map(str::trim)
            && value.starts_with('"')
            && value.ends_with('"')
            && value.len() >= 2
        {
            return Some(value[1..value.len() - 1].to_string());
        }
    }

    None
}

const KERNEL_GIT_URL: &str = "https://github.com/petitstrawberry/Scarlet";
const KERNEL_DEFAULT_REV: &str = "v0.17.0";

fn new_scaffold(
    module: Option<String>,
    project: Option<String>,
    kernel_path: Option<&Path>,
    kernel_rev: Option<&str>,
    target: Option<&str>,
) -> Result<(), String> {
    match (module, project) {
        (Some(name), None) => scaffold_module(&name, kernel_path, kernel_rev),
        (None, Some(name)) => scaffold_project(&name, kernel_path, kernel_rev, target),
        (Some(_), Some(_)) => Err("cannot specify both --module and --project".to_string()),
        (None, None) => Err("specify --module or --project".to_string()),
    }
}

fn kernel_dependency_spec(
    kernel_path: Option<&Path>,
    kernel_rev: Option<&str>,
    module_dir: &Path,
) -> Result<String, String> {
    if let Some(path) = kernel_path {
        let abs_kernel = fs::canonicalize(path).map_err(|e| format!("{e}: {}", path.display()))?;
        let abs_module = module_dir.to_path_buf();
        let rel = pathdiff(&abs_kernel, &abs_module)?;
        Ok(format!("path = \"{}\"", rel.display()))
    } else {
        let rev = kernel_rev.unwrap_or(KERNEL_DEFAULT_REV);
        Ok(format!("git = \"{KERNEL_GIT_URL}\", rev = \"{rev}\""))
    }
}

fn kernel_source_toml(
    kernel_path: Option<&Path>,
    _kernel_rev: Option<&str>,
    base_dir: &Path,
) -> Result<String, String> {
    if let Some(path) = kernel_path {
        let abs_kernel = fs::canonicalize(path).map_err(|e| format!("{e}: {}", path.display()))?;
        let rel = pathdiff(&abs_kernel, base_dir)?;
        Ok(rel.display().to_string())
    } else {
        Ok("../../kernel".to_string())
    }
}

fn scaffold_module(
    name: &str,
    kernel_path: Option<&Path>,
    kernel_rev: Option<&str>,
) -> Result<(), String> {
    let module_dir = PathBuf::from(name);
    let kernel_spec = kernel_dependency_spec(kernel_path, kernel_rev, &module_dir)?;
    let crate_name = cargo_key_to_rust_identifier(name);
    let src_dir = module_dir.join("src");
    let cargo_dir = module_dir.join(".cargo");

    fs::create_dir_all(&src_dir)
        .map_err(|e| format!("failed to create {}: {e}", src_dir.display()))?;
    fs::create_dir_all(&cargo_dir)
        .map_err(|e| format!("failed to create {}: {e}", cargo_dir.display()))?;

    let name_bytes = name.as_bytes();
    let name_with_null_len = name_bytes.len() + 1;

    let cargo_toml = format!(
        r#"[package]
name = "{crate_name}"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"

[dependencies]
scarlet = {{ {kernel_spec} }}
"#
    );
    write_if_changed(&module_dir.join("Cargo.toml"), &cargo_toml)?;

    let module_toml = format!(
        r#"[module]
name = "{name}"
depends = []
"#
    );
    write_if_changed(&module_dir.join("module.toml"), &module_toml)?;

    let build_rs = r#"use std::path::Path;

fn parse_depends(content: &str) -> Vec<String> {
    let mut deps = Vec::new();
    let mut rest = content;

    while let Some(depends_pos) = rest.find("depends") {
        rest = &rest[depends_pos + "depends".len()..];
        let Some(eq_pos) = rest.find('=') else {
            break;
        };
        rest = &rest[eq_pos + 1..];
        let Some(open_pos) = rest.find('[') else {
            break;
        };
        rest = &rest[open_pos + 1..];
        let Some(close_pos) = rest.find(']') else {
            break;
        };

        let array = &rest[..close_pos];
        for item in array.split(',') {
            let trimmed = item.trim();
            if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
                deps.push(trimmed[1..trimmed.len() - 1].to_string());
            }
        }
        break;
    }

    deps
}

fn main() {
    let rustc_version = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let output = std::process::Command::new(rustc_version)
        .arg("--version")
        .output()
        .expect("failed to run rustc --version");
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    println!("cargo:rustc-env=RUSTC_VERSION={version}");

    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=TARGET={target}");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let module_toml = Path::new(&manifest_dir).join("module.toml");
    println!("cargo:rerun-if-changed={}", module_toml.display());

    if module_toml.exists() {
        let content = std::fs::read_to_string(&module_toml).expect("failed to read module.toml");
        let depends = parse_depends(&content);
        println!("cargo:rustc-env=SCARLET_LSM_DEPENDS={}", depends.join(","));
    } else {
        println!("cargo:rustc-env=SCARLET_LSM_DEPENDS=");
    }
}
"#;
    write_if_changed(&module_dir.join("build.rs"), build_rs)?;

    let lib_rs = format!(
        r#"#![no_std]

use scarlet::early_println;

#[unsafe(no_mangle)]
pub static SCARLET_LSM_NAME: [u8; {name_with_null_len}] = *b"{name}\0";

#[unsafe(no_mangle)]
pub static SCARLET_LSM_BUILD_INFO: [u8; 72] = {{
    let s = concat!(env!("RUSTC_VERSION"), ";", env!("TARGET"), "\0");
    let bytes: &[u8] = s.as_bytes();
    let mut arr = [0u8; 72];
    let mut i = 0;
    while i < bytes.len() && i < 72 {{
        arr[i] = bytes[i];
        i += 1;
    }}
    arr
}};

#[unsafe(no_mangle)]
pub static SCARLET_LSM_DEPENDS: [u8; 256] = {{
    let s = concat!(env!("SCARLET_LSM_DEPENDS"), "\0");
    let bytes: &[u8] = s.as_bytes();
    let mut arr = [0u8; 256];
    let mut i = 0;
    while i < bytes.len() && i < 256 {{
        arr[i] = bytes[i];
        i += 1;
    }}
    arr
}};

#[unsafe(no_mangle)]
pub extern "C" fn scarlet_lsm_init() -> Result<(), &'static str> {{
    early_println!("[{name}] loaded!");
    Ok(())
}}
"#
    );
    write_if_changed(&src_dir.join("lib.rs"), &lib_rs)?;

    let cargo_config = r#"[target.riscv64gc-unknown-none-elf]
runner = "true"

[target.aarch64-unknown-none-elf]
runner = "true"

[profile.dev]
opt-level = 3

[unstable]
build-std = ["core", "compiler_builtins", "alloc"]
build-std-features = ["compiler-builtins-mem"]
unstable-options = true
"#;
    write_if_changed(&cargo_dir.join("config.toml"), cargo_config)?;

    let _ = write_if_changed(&module_dir.join(".gitignore"), "target/\n");

    eprintln!("cargo-scarlet: created loadable module '{name}'");
    Ok(())
}

fn render_project_build_rs() -> String {
    "fn main() {}\n".to_string()
}

fn scaffold_project(
    name: &str,
    kernel_path: Option<&Path>,
    kernel_rev: Option<&str>,
    target: Option<&str>,
) -> Result<(), String> {
    let target = target.ok_or("--target is required for project")?;
    let project_dir = PathBuf::from(name);
    let kernel_spec = kernel_dependency_spec(kernel_path, kernel_rev, &project_dir)?;
    let kernel_source = kernel_source_toml(kernel_path, kernel_rev, &project_dir)?;
    let target_json_dir = match kernel_path {
        Some(p) => {
            let abs = fs::canonicalize(p).map_err(|e| format!("{e}: {}", p.display()))?;
            let rel = pathdiff(&abs, &project_dir)?;
            format!("{}/targets/{}", rel.display(), target)
        }
        None => format!("../../kernel/targets/{target}"),
    };
    let src_dir = project_dir.join("src");
    let lds_dir = project_dir.join("lds");
    let cargo_dir = project_dir.join(".cargo");
    let scarlet_modules_dir = project_dir.join(".scarlet/scarlet-modules/src");

    fs::create_dir_all(&src_dir)
        .map_err(|e| format!("failed to create {}: {e}", src_dir.display()))?;
    fs::create_dir_all(&lds_dir)
        .map_err(|e| format!("failed to create {}: {e}", lds_dir.display()))?;
    fs::create_dir_all(&cargo_dir)
        .map_err(|e| format!("failed to create {}: {e}", cargo_dir.display()))?;
    fs::create_dir_all(&scarlet_modules_dir)
        .map_err(|e| format!("failed to create {}: {e}", scarlet_modules_dir.display()))?;

    let crate_name = cargo_key_to_rust_identifier(name);

    let build_rs = render_project_build_rs();
    write_if_changed(&project_dir.join("build.rs"), &build_rs)?;
    let main_rs = r#"#![no_std]
#![no_main]

extern crate scarlet_modules;

use scarlet_modules::scarlet;

#[unsafe(link_section = ".init")]
#[unsafe(no_mangle)]
pub extern "C" fn arch_start_kernel() -> ! {{
    scarlet_modules::force_link();
    // REQUIRED: implement architecture-specific boot entry
    // e.g. scarlet_modules::scarlet::arch::riscv64::boot::limine::limine_entry()
    loop {{}}
}}
"#
    .to_string();
    write_if_changed(&src_dir.join("main.rs"), &main_rs)?;

    let project_cargo_toml = format!(
        r#"[package]
name = "{crate_name}"
version = "0.1.0"
edition = "2024"

[[bin]]
name = "scarlet"
path = "src/main.rs"

[dependencies]
scarlet_modules = {{ package = "scarlet-modules", path = ".scarlet/scarlet-modules" }}
"#
    );
    write_if_changed(&project_dir.join("Cargo.toml"), &project_cargo_toml)?;

    let scarlet_manifest = format!(
        r#"schema_version = 2

[project]
name = "{name}"

[kernel]
package = "scarlet"
source = "{kernel_source}"
target = "{target}"
target_json = "{target_json_dir}"
"#
    );
    write_if_changed(&project_dir.join("scarlet.toml"), &scarlet_manifest)?;

    let cargo_config = format!(
        r#"[profile.dev]
opt-level = 3

[profile.test]
opt-level = 3

[build]
target = "{target_json_dir}"

[unstable]
build-std = ["core", "compiler_builtins", "alloc"]
build-std-features = ["compiler-builtins-mem"]
unstable-options = true
"#
    );
    write_if_changed(&cargo_dir.join("config.toml"), &cargo_config)?;

    let modules_cargo_toml = format!(
        r#"# generated by cargo-scarlet

[package]
name = "scarlet-modules"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"

[dependencies]
scarlet = {{ {kernel_spec}, default-features = false }}
"#
    );
    write_if_changed(
        &project_dir.join(".scarlet/scarlet-modules/Cargo.toml"),
        &modules_cargo_toml,
    )?;

    let modules_lib_rs = r#"#![no_std]

pub use scarlet;

#[inline(never)]
pub fn force_link() {}
"#;
    write_if_changed(
        &project_dir.join(".scarlet/scarlet-modules/src/lib.rs"),
        modules_lib_rs,
    )?;

    let _ = write_if_changed(&project_dir.join(".gitignore"), ".scarlet\ntarget\n");

    let modules_cargo_dir = project_dir.join(".scarlet/scarlet-modules/.cargo");
    fs::create_dir_all(&modules_cargo_dir)
        .map_err(|e| format!("failed to create {}: {e}", modules_cargo_dir.display()))?;
    let modules_cargo_config = render_cargo_config();
    fs::write(modules_cargo_dir.join("config.toml"), modules_cargo_config)
        .map_err(|e| format!("failed to write scarlet-modules .cargo/config.toml: {e}"))?;

    eprintln!("cargo-scarlet: created project '{name}'");
    eprintln!("cargo-scarlet: REQUIRED: update .cargo/config.toml with runner");
    eprintln!(
        "cargo-scarlet: REQUIRED: update .scarlet/scarlet-modules/.cargo/config.toml with target and build-std"
    );
    eprintln!("cargo-scarlet: REQUIRED: add linker script to lds/");
    eprintln!("cargo-scarlet: REQUIRED: implement boot entry in src/main.rs (arch_start_kernel)");

    Ok(())
}

fn cross_tools_for_target(target_triple: &str) -> (String, String) {
    let candidates: &[(&str, &[&str])] = &[
        (
            "riscv64",
            &[
                "riscv64-unknown-linux-gnu",
                "riscv64-linux-gnu",
                "riscv64-unknown-elf",
            ],
        ),
        (
            "aarch64",
            &[
                "aarch64-unknown-linux-gnu",
                "aarch64-linux-gnu",
                "aarch64-none-elf",
            ],
        ),
        ("x86_64", &["x86_64-unknown-linux-gnu", "x86_64-linux-gnu"]),
    ];

    let prefixes = candidates
        .iter()
        .find(|(arch, _)| target_triple.starts_with(arch))
        .map(|(_, prefixes)| *prefixes)
        .unwrap_or(&[]);

    for prefix in prefixes {
        let nm = format!("{prefix}-nm");
        let objcopy = format!("{prefix}-objcopy");
        if which(&nm) && which(&objcopy) {
            return (nm, objcopy);
        }
    }

    if which("llvm-nm") && which("llvm-objcopy") {
        return ("llvm-nm".to_string(), "llvm-objcopy".to_string());
    }

    ("nm".to_string(), "objcopy".to_string())
}

fn which(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata_with_kernel_features(features: &[&str]) -> CargoMetadata {
        CargoMetadata {
            packages: vec![CargoMetadataPackage {
                id: "path+file:///kernel#scarlet@0.16.0".to_string(),
                name: "scarlet".to_string(),
            }],
            resolve: Some(CargoResolve {
                nodes: vec![CargoResolveNode {
                    id: "path+file:///kernel#scarlet@0.16.0".to_string(),
                    features: features
                        .iter()
                        .map(|feature| (*feature).to_string())
                        .collect(),
                }],
            }),
        }
    }

    #[test]
    fn feature_states_separate_enabled_and_disabled_features() {
        let features = KernelFeatureConfig::States(BTreeMap::from([
            ("hypervisor".to_string(), false),
            ("limine".to_string(), true),
        ]));

        assert_eq!(features.enabled(), vec!["limine"]);
        assert_eq!(features.disabled(), vec!["hypervisor"]);
    }

    #[test]
    fn explicitly_disabled_kernel_feature_rejects_cargo_conflict() {
        let metadata = metadata_with_kernel_features(&["hypervisor", "limine"]);

        let error =
            ensure_disabled_kernel_features(&metadata, "scarlet", &["hypervisor".to_string()])
                .expect_err("hypervisor must conflict");

        assert!(error.contains("hypervisor"));
    }

    #[test]
    fn explicitly_disabled_kernel_feature_allows_absent_feature() {
        let metadata = metadata_with_kernel_features(&["limine"]);

        ensure_disabled_kernel_features(&metadata, "scarlet", &["hypervisor".to_string()])
            .expect("absent hypervisor must be accepted");
    }

    #[test]
    fn lock_source_path_serializes_to_table() {
        let source = LockPackageSource::path("../../user/bin".to_string());
        let lock = PackageLock {
            kind: "cargo".to_string(),
            source: Some(source.clone()),
            git: None,
            git_ref: None,
            resolved_rev: None,
            package: None,
            bin: Some("sh".to_string()),
            features: Vec::new(),
            default_features: None,
            to: "/bin/sh".to_string(),
            output: None,
            hash: "sha256:abc".to_string(),
        };
        let toml_str = toml::to_string_pretty(&lock).unwrap();
        assert!(toml_str.contains("type = \"path\""), "expected type tag");
        assert!(
            toml_str.contains("../../user/bin"),
            "expected relative path"
        );
        assert!(
            !toml_str.contains("/Users/"),
            "should not contain absolute path"
        );

        // Round-trip
        let deserialized: PackageLock = toml::from_str(&toml_str).unwrap();
        assert_eq!(deserialized.source, Some(source));
    }

    #[test]
    fn lock_source_git_serializes_to_table() {
        let source = LockPackageSource::git(
            "https://github.com/example/repo".to_string(),
            "abc123".to_string(),
        );
        let lock = PackageLock {
            kind: "cargo".to_string(),
            source: Some(source.clone()),
            git: Some("https://github.com/example/repo".to_string()),
            git_ref: Some("refs/heads/main".to_string()),
            resolved_rev: Some("abc123".to_string()),
            package: None,
            bin: Some("tool".to_string()),
            features: Vec::new(),
            default_features: None,
            to: "/bin/tool".to_string(),
            output: None,
            hash: "sha256:def".to_string(),
        };
        let toml_str = toml::to_string_pretty(&lock).unwrap();
        assert!(toml_str.contains("type = \"git\""), "expected git type tag");
        assert!(toml_str.contains("https://github.com/example/repo"));

        let deserialized: PackageLock = toml::from_str(&toml_str).unwrap();
        assert_eq!(deserialized.source, Some(source));
    }

    #[test]
    fn lock_legacy_string_source_deserializes() {
        let toml_str = r#"
kind = "cargo"
source = "/Users/someone/absolute/path"
bin = "sh"
hash = "sha256:abc"
"#;
        let lock: PackageLock = toml::from_str(toml_str).unwrap();
        match lock.source {
            Some(LockPackageSource::LegacyPath(s)) => {
                assert_eq!(s, "/Users/someone/absolute/path");
            }
            other => panic!("expected LegacyPath, got {other:?}"),
        }
    }

    #[test]
    fn bundle_cargo_layer_accepts_feature_controls() {
        let toml_str = r#"
[[layers]]
kind = "cargo"
source = "../../user/video_player"
package = "video_player"
bin = "video_player"
default-features = false
features = ["h264-stateful-hw", "mp4-aac"]
replace = true
to = "/system/scarlet/bin/video_player"
"#;
        let bundle: BundleManifest = toml::from_str(toml_str).unwrap();
        let ManifestLayer::Cargo {
            features,
            default_features,
            replace,
            ..
        } = &bundle.layers[0]
        else {
            panic!("expected cargo layer");
        };

        assert_eq!(default_features, &Some(false));
        assert!(*replace);
        assert_eq!(
            features,
            &vec!["h264-stateful-hw".to_string(), "mp4-aac".to_string()]
        );
    }

    #[test]
    fn cargo_layer_replace_removes_previous_same_destination() {
        let layers = vec![
            ManifestLayer::Cargo {
                source: PackageSource::Path("user/video_player".to_string()),
                package: Some("video_player".to_string()),
                bin: Some("video_player".to_string()),
                features: vec!["h264-stateful-hw".to_string(), "mp4-aac".to_string()],
                default_features: Some(false),
                replace: false,
                to: "/system/scarlet/bin/video_player".to_string(),
            },
            ManifestLayer::Cargo {
                source: PackageSource::Path("user/video_player".to_string()),
                package: Some("video_player".to_string()),
                bin: Some("video_player".to_string()),
                features: vec![
                    "h264-stateful-hw".to_string(),
                    "h264-stateless-hw".to_string(),
                    "mp4-aac".to_string(),
                ],
                default_features: Some(false),
                replace: true,
                to: "/system/scarlet/bin/video_player".to_string(),
            },
        ];
        let ctx = TemplateContext {
            arch: "aarch64".to_string(),
            target_triple: "aarch64-unknown-none-elf".to_string(),
            project: "test".to_string(),
        };
        let images = BTreeMap::new();

        let resolved = resolve_layers(&layers, Path::new("/tmp/scarlet"), &ctx, &images).unwrap();
        let packages: Vec<_> = resolved
            .iter()
            .filter_map(|layer| match layer {
                ResolvedLayer::Package(pkg) => Some(pkg),
                _ => None,
            })
            .collect();

        assert_eq!(packages.len(), 1);
        assert_eq!(
            packages[0].features,
            vec![
                "h264-stateful-hw".to_string(),
                "h264-stateless-hw".to_string(),
                "mp4-aac".to_string(),
            ]
        );
    }

    #[test]
    fn nested_bundle_cargo_replace_removes_previous_destination() {
        let temp = std::env::temp_dir().join(format!(
            "cargo-scarlet-nested-replace-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(temp.join("bundles/desktop")).unwrap();
        fs::create_dir_all(temp.join("bundles/experimental")).unwrap();
        fs::write(
            temp.join("bundles/desktop/bundle.toml"),
            r#"
[[layers]]
kind = "cargo"
source = "../../user/video_player"
package = "video_player"
bin = "video_player"
default-features = false
features = ["av1-stateful-hw", "h264-stateful-hw", "mp4-aac"]
to = "/system/scarlet/bin/video_player"
"#,
        )
        .unwrap();
        fs::write(
            temp.join("bundles/experimental/bundle.toml"),
            r#"
[[layers]]
kind = "cargo"
source = "../../user/video_player"
package = "video_player"
bin = "video_player"
default-features = false
features = ["av1-stateful-hw", "h264-stateful-hw", "h264-stateless-hw", "mp4-aac"]
replace = true
to = "/system/scarlet/bin/video_player"
"#,
        )
        .unwrap();

        let layers = vec![
            ManifestLayer::Bundle {
                path: Some("bundles/desktop/bundle.toml".to_string()),
                source: None,
                subdir: None,
                bundle: None,
            },
            ManifestLayer::Bundle {
                path: Some("bundles/experimental/bundle.toml".to_string()),
                source: None,
                subdir: None,
                bundle: None,
            },
        ];
        let ctx = TemplateContext {
            arch: "aarch64".to_string(),
            target_triple: "aarch64-unknown-none-elf".to_string(),
            project: "test".to_string(),
        };
        let images = BTreeMap::new();

        let resolved = resolve_layers(&layers, &temp, &ctx, &images).unwrap();
        let packages: Vec<_> = resolved
            .iter()
            .filter_map(|layer| match layer {
                ResolvedLayer::Package(pkg) => Some(pkg),
                _ => None,
            })
            .collect();

        assert_eq!(packages.len(), 1);
        assert_eq!(
            packages[0].features,
            vec![
                "av1-stateful-hw".to_string(),
                "h264-stateful-hw".to_string(),
                "h264-stateless-hw".to_string(),
                "mp4-aac".to_string(),
            ]
        );

        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn package_lock_matches_all_input_fields() {
        let project = Path::new("/Users/foo/project");
        let lock = PackageLock {
            kind: "cargo".to_string(),
            source: Some(LockPackageSource::path("src".to_string())),
            git: None,
            git_ref: None,
            resolved_rev: None,
            package: Some("apps".to_string()),
            bin: Some("sh".to_string()),
            features: vec!["h264-stateful-hw".to_string()],
            default_features: Some(false),
            to: "/bin/sh".to_string(),
            output: None,
            hash: String::new(),
        };
        let mut pkg = ResolvedPackage {
            kind: Some("cargo".to_string()),
            source: Some(PackageSource::Path("src".to_string())),
            local_source: Some(project.join("src")),
            resolved_rev: None,
            package_name: Some("apps".to_string()),
            bin: Some("sh".to_string()),
            features: vec!["h264-stateful-hw".to_string()],
            default_features: Some(false),
            from: None,
            to: "/bin/sh".to_string(),
            output: None,
        };

        assert!(package_lock_matches_input(project, &lock, &pkg).unwrap());

        pkg.to = "/usr/bin/sh".to_string();
        assert!(!package_lock_matches_input(project, &lock, &pkg).unwrap());
        pkg.to = "/bin/sh".to_string();
        pkg.features = vec!["vp9-stateless-hw".to_string()];
        assert!(!package_lock_matches_input(project, &lock, &pkg).unwrap());
    }

    #[test]
    fn copy_lock_matches_destination_and_template() {
        let lock = FileLock {
            source: "https://example.com/archive.tar".to_string(),
            to: "/opt/archive.tar".to_string(),
            template: false,
            hash: "sha256:abc".to_string(),
        };
        let mut file = ResolvedFile {
            source: FileSource::Url("https://example.com/archive.tar".to_string()),
            to: "/opt/archive.tar".to_string(),
            template: false,
        };

        assert!(copy_lock_matches_input(&lock, &file));

        file.template = true;
        assert!(!copy_lock_matches_input(&lock, &file));
    }

    #[test]
    fn copy_layer_creates_intermediate_directories() {
        let temp = std::env::temp_dir().join(format!(
            "cargo-scarlet-copy-layer-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp);
        let source = temp.join("source");
        let staging = temp.join("staging");
        let cache = temp.join("cache");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&staging).unwrap();

        let file = ResolvedFile {
            source: FileSource::Local(source),
            to: "/data/config/system/linux-aarch64".to_string(),
            template: true,
        };
        let tpl_ctx = TemplateContext {
            arch: "aarch64".to_string(),
            target_triple: "aarch64-unknown-none-elf".to_string(),
            project: "test".to_string(),
        };
        let mut layer_locks = Vec::new();

        apply_copy_layer(
            &file,
            &staging,
            &cache,
            None,
            &tpl_ctx,
            false,
            &mut layer_locks,
        )
        .unwrap();

        assert!(staging.join("data/config/system/linux-aarch64").is_dir());
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn copy_dir_replaces_existing_dangling_symlink() {
        let temp =
            std::env::temp_dir().join(format!("cargo-scarlet-symlink-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        let source = temp.join("source");
        let dest = temp.join("dest");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();
        std::os::unix::fs::symlink("/missing-target", source.join("udevadm")).unwrap();
        std::os::unix::fs::symlink("/old-missing-target", dest.join("udevadm")).unwrap();

        copy_dir_recursive(&source, &dest).unwrap();

        assert_eq!(
            fs::read_link(dest.join("udevadm")).unwrap(),
            Path::new("/missing-target")
        );
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn newc_writer_preserves_symlinks() {
        let temp = std::env::temp_dir().join(format!(
            "cargo-scarlet-newc-symlink-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp);
        let staging = temp.join("staging");
        fs::create_dir_all(staging.join("bin")).unwrap();
        std::os::unix::fs::symlink("busybox", staging.join("bin/sh")).unwrap();
        let output = temp.join("initramfs.cpio");

        build_initramfs_newc_from_staging(&staging, &output).unwrap();
        let archive = fs::read(&output).unwrap();
        let name = b"bin/sh\0";
        let name_pos = archive
            .windows(name.len())
            .position(|window| window == name)
            .expect("symlink path missing from archive");
        let header_start = name_pos - 110;
        let header = core::str::from_utf8(&archive[header_start..header_start + 110]).unwrap();

        let mode = u32::from_str_radix(&header[14..22], 16).unwrap();
        assert_eq!(mode & 0o170000, 0o120000);
        assert_eq!(&header[54..62], "00000007");
        assert!(
            archive
                .windows(b"busybox".len())
                .any(|window| window == b"busybox")
        );

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn gpt_ext2_image_contains_root_partition() {
        if !command_exists("mke2fs") {
            eprintln!("skipping gpt-ext2 image test: mke2fs not found");
            return;
        }

        let temp = std::env::temp_dir().join(format!(
            "cargo-scarlet-gpt-ext2-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp);
        let staging = temp.join("staging");
        fs::create_dir_all(staging.join("etc")).unwrap();
        fs::write(staging.join("etc/issue"), "Scarlet\n").unwrap();
        let output = temp.join("rootfs.img");

        build_gpt_ext2_from_staging(&staging, &output, "rootfs").unwrap();

        let disk = gpt::GptConfig::new().open(&output).unwrap();
        let partition = disk.partitions().get(&1).expect("missing partition 1");
        assert_eq!(partition.name, "SCARLET_ROOT");
        assert_eq!(partition.first_lba, GPT_FIRST_PARTITION_LBA);
        assert_eq!(partition.part_type_guid, gpt::partition_types::LINUX_FS);

        let image_size = fs::metadata(&output).unwrap().len();
        assert!(image_size > partition.last_lba * GPT_SECTOR_SIZE);

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn gpt_image_composes_partition_payloads() {
        let temp = std::env::temp_dir().join(format!(
            "cargo-scarlet-gpt-compose-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp);
        let images_dir = temp.join("images");
        fs::create_dir_all(&images_dir).unwrap();
        fs::write(images_dir.join("boot.img"), b"boot-payload").unwrap();
        fs::write(images_dir.join("rootfs.ext2"), b"root-payload").unwrap();

        let mut images = BTreeMap::new();
        images.insert(
            "boot".to_string(),
            ManifestImageSection {
                output: Some("images/boot.img".to_string()),
                ..Default::default()
            },
        );
        images.insert(
            "rootfs".to_string(),
            ManifestImageSection {
                output: Some("images/rootfs.ext2".to_string()),
                ..Default::default()
            },
        );
        let partitions = vec![
            ManifestGptPartition {
                source: "boot".to_string(),
                name: "SCARLET_BOOT".to_string(),
                type_name: "efi-system".to_string(),
                flags: 0,
                alignment_lba: None,
            },
            ManifestGptPartition {
                source: "rootfs".to_string(),
                name: "SCARLET_ROOT".to_string(),
                type_name: "linux-filesystem".to_string(),
                flags: 0,
                alignment_lba: None,
            },
        ];
        let output = images_dir.join("disk.img");

        build_gpt_image_from_partitions(&temp, &images, &partitions, &output, "disk").unwrap();

        let disk = gpt::GptConfig::new().open(&output).unwrap();
        let boot = disk.partitions().get(&1).expect("missing boot partition");
        assert_eq!(boot.name, "SCARLET_BOOT");
        assert_eq!(boot.first_lba, GPT_FIRST_PARTITION_LBA);
        assert_eq!(boot.part_type_guid, gpt::partition_types::EFI);
        let root = disk.partitions().get(&2).expect("missing root partition");
        assert_eq!(root.name, "SCARLET_ROOT");
        assert_eq!(root.first_lba, GPT_FIRST_PARTITION_LBA * 2);
        assert_eq!(root.part_type_guid, gpt::partition_types::LINUX_FS);

        let mut disk_file = fs::File::open(&output).unwrap();
        let mut boot_payload = vec![0; b"boot-payload".len()];
        disk_file
            .seek(SeekFrom::Start(boot.first_lba * GPT_SECTOR_SIZE))
            .unwrap();
        disk_file.read_exact(&mut boot_payload).unwrap();
        assert_eq!(&boot_payload, b"boot-payload");

        let mut root_payload = vec![0; b"root-payload".len()];
        disk_file
            .seek(SeekFrom::Start(root.first_lba * GPT_SECTOR_SIZE))
            .unwrap();
        disk_file.read_exact(&mut root_payload).unwrap();
        assert_eq!(&root_payload, b"root-payload");

        let _ = fs::remove_dir_all(&temp);
    }

    fn command_exists(command: &str) -> bool {
        Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {command} >/dev/null 2>&1"))
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[test]
    fn image_stamp_must_match_hash_and_output() {
        let temp = std::env::temp_dir().join(format!(
            "cargo-scarlet-image-stamp-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        let output = temp.join("images/rootfs.ext2");
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::write(&output, b"rootfs").unwrap();

        let lock = SectionLock {
            hash: "sha256:new".to_string(),
            layers: Vec::new(),
            files: Vec::new(),
            packages: Vec::new(),
        };

        assert!(!image_output_is_current(
            &temp,
            "rootfs",
            &output,
            "sha256:new",
            Some(&lock)
        ));

        write_image_stamp(&temp, "rootfs", &output, "sha256:new").unwrap();
        assert!(image_output_is_current(
            &temp,
            "rootfs",
            &output,
            "sha256:new",
            Some(&lock)
        ));
        assert!(!image_output_is_current(
            &temp,
            "rootfs",
            &output,
            "sha256:old",
            Some(&lock)
        ));
        assert!(!image_output_is_current(
            &temp,
            "rootfs",
            &temp.join("images/other.ext2"),
            "sha256:new",
            Some(&lock)
        ));

        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn normalize_path_lexically_cleans_dots() {
        let input = Path::new("/Users/foo/projects/riscv64/../../bundles/base/../../user/bin");
        let normalized = normalize_path_lexically(input);
        assert_eq!(normalized, Path::new("/Users/foo/user/bin"));
    }

    #[test]
    fn pathdiff_produces_stable_relative() {
        let project = Path::new("/Users/foo/projects/riscv64-limine-full");
        let source = Path::new("/Users/foo/user/bin");
        let relative = pathdiff(source, project).unwrap();
        assert_eq!(relative, Path::new("../../user/bin"));
    }

    #[test]
    fn git_bundle_source_recursively_expands_nested_layers() {
        if !Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            eprintln!("skipping: git binary not available");
            return;
        }
        fn run_git(dir: &Path, args: &[&str]) {
            let status = Command::new("git")
                .args(args)
                .current_dir(dir)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed in {}", dir.display());
        }

        let temp = std::env::temp_dir().join(format!(
            "cargo-scarlet-git-bundle-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp);
        let project = temp.join("project");
        let repo = temp.join("bundle-repo");
        fs::create_dir_all(repo.join("bundles/base/fs")).unwrap();
        fs::create_dir_all(repo.join("bundles/desktop")).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::write(repo.join("bundles/base/fs/.keep"), "").unwrap();
        fs::write(
            repo.join("bundles/base/bundle.toml"),
            r#"
[[layers]]
kind = "copy"
source = "fs"
to = "/"
"#,
        )
        .unwrap();
        fs::write(
            repo.join("bundles/desktop/bundle.toml"),
            r#"
[[layers]]
kind = "bundle"
path = "../base/bundle.toml"
"#,
        )
        .unwrap();
        run_git(&repo, &["init", "--quiet"]);
        run_git(&repo, &["config", "user.email", "test@example.com"]);
        run_git(&repo, &["config", "user.name", "cargo-scarlet test"]);
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "--quiet", "-m", "bundle test"]);
        let revision = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repo)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();

        let manifest = format!(
            r#"
[[layers]]
kind = "bundle"
source = {{ git = "{}", rev = "{}" }}
subdir = "bundles/desktop"
"#,
            repo.display(),
            revision.trim()
        );
        let bundle: BundleManifest = toml::from_str(&manifest).unwrap();
        let ctx = TemplateContext {
            arch: "aarch64".to_string(),
            target_triple: "aarch64-unknown-none-elf".to_string(),
            project: "test".to_string(),
        };
        let images = BTreeMap::new();

        let resolved = resolve_layers(&bundle.layers, &project, &ctx, &images).unwrap();
        assert_eq!(resolved.len(), 1);
        match &resolved[0] {
            ResolvedLayer::Copy(ResolvedFile {
                source: FileSource::Local(path),
                ..
            }) => {
                let path = fs::canonicalize(path).unwrap();
                let git_cache_dir = fs::canonicalize(project.join(".scarlet/cache/git")).unwrap();
                assert!(path.starts_with(git_cache_dir));
                assert!(path.ends_with(Path::new("bundles/base/fs")));
            }
            _ => panic!("expected copy layer from nested git bundle"),
        }

        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn bundle_layer_expands_in_place() {
        let temp = std::env::temp_dir().join(format!("cargo-scarlet-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(temp.join("bundle")).unwrap();
        fs::write(
            temp.join("bundle/bundle.toml"),
            r#"
[[layers]]
kind = "copy"
source = "fs"
to = "/"
"#,
        )
        .unwrap();

        let layers = vec![
            ManifestLayer::Bundle {
                path: Some("bundle/bundle.toml".to_string()),
                source: None,
                subdir: None,
                bundle: None,
            },
            ManifestLayer::Copy {
                source: "rootfs".to_string(),
                to: "/".to_string(),
                template: false,
            },
        ];
        let ctx = TemplateContext {
            arch: "aarch64".to_string(),
            target_triple: "aarch64-unknown-none-elf".to_string(),
            project: "test".to_string(),
        };
        let images = BTreeMap::new();

        let resolved = resolve_layers(&layers, &temp, &ctx, &images).unwrap();
        assert_eq!(resolved.len(), 2);
        match &resolved[0] {
            ResolvedLayer::Copy(file) => match &file.source {
                FileSource::Local(path) => assert_eq!(path, &temp.join("bundle/fs")),
                other => panic!("expected local source, got {other:?}"),
            },
            _ => panic!("expected first layer to be bundle copy"),
        }
        match &resolved[1] {
            ResolvedLayer::Copy(file) => match &file.source {
                FileSource::Local(path) => assert_eq!(path, &temp.join("rootfs")),
                other => panic!("expected local source, got {other:?}"),
            },
            _ => panic!("expected second layer to be project copy"),
        }

        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn limine_hash_includes_extra_package_destination_source_and_contents() {
        let temp = std::env::temp_dir().join(format!(
            "cargo-scarlet-limine-package-hash-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        let kernel = temp.join("kernel.elf");
        let initramfs = temp.join("initramfs.cpio");
        let overlay = temp.join("apple-avd.dtbo");
        fs::write(&kernel, b"kernel").unwrap();
        fs::write(&initramfs, b"initramfs").unwrap();
        fs::write(&overlay, b"first overlay").unwrap();

        let package = PluginRequestPackage {
            source: overlay.display().to_string(),
            to: "/boot/apple-avd.dtbo".to_string(),
        };
        let first = limine_image_hash("", &kernel, &initramfs, None, &[package]).unwrap();
        fs::write(&overlay, b"changed overlay").unwrap();
        let changed_contents = limine_image_hash(
            "",
            &kernel,
            &initramfs,
            None,
            &[PluginRequestPackage {
                source: overlay.display().to_string(),
                to: "/boot/apple-avd.dtbo".to_string(),
            }],
        )
        .unwrap();
        assert_ne!(first, changed_contents);

        let changed_destination = limine_image_hash(
            "",
            &kernel,
            &initramfs,
            None,
            &[PluginRequestPackage {
                source: overlay.display().to_string(),
                to: "/boot/avd.dtbo".to_string(),
            }],
        )
        .unwrap();
        assert_ne!(changed_contents, changed_destination);

        let copied_overlay = temp.join("copied.dtbo");
        fs::write(&copied_overlay, b"changed overlay").unwrap();
        let changed_source = limine_image_hash(
            "",
            &kernel,
            &initramfs,
            None,
            &[PluginRequestPackage {
                source: copied_overlay.display().to_string(),
                to: "/boot/avd.dtbo".to_string(),
            }],
        )
        .unwrap();
        assert_ne!(changed_destination, changed_source);

        fs::remove_dir_all(&temp).unwrap();
    }

    fn archive_test_dir(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "cargo-scarlet-{name}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn sha256_bytes(bytes: &[u8]) -> String {
        format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
    }

    fn tar_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Cursor;

        let mut builder = tar::Builder::new(Vec::new());
        for (path, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, Cursor::new(content.to_vec()))
                .unwrap();
        }
        builder.finish().unwrap();
        builder.into_inner().unwrap()
    }

    fn tar_bytes_with_raw_path(path: &str, content: &[u8]) -> Vec<u8> {
        let mut bytes = tar_bytes(&[("safe", content)]);
        assert!(path.len() < 100);
        bytes[..100].fill(0);
        bytes[..path.len()].copy_from_slice(path.as_bytes());
        update_tar_header_checksum(&mut bytes);
        bytes
    }

    fn tar_bytes_with_raw_type(entry_type: u8) -> Vec<u8> {
        let mut bytes = tar_bytes(&[("safe", b"content")]);
        bytes[156] = entry_type;
        update_tar_header_checksum(&mut bytes);
        bytes
    }

    fn update_tar_header_checksum(bytes: &mut [u8]) {
        bytes[148..156].fill(b' ');
        let checksum = bytes[..512]
            .iter()
            .fold(0u32, |sum, byte| sum + u32::from(*byte));
        let encoded = format!("{checksum:06o}\0 ");
        bytes[148..156].copy_from_slice(encoded.as_bytes());
    }

    fn tar_link_bytes(path: &str, target: &str, entry_type: tar::EntryType) -> Vec<u8> {
        use std::io::Cursor;

        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(entry_type);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_link_name(target).unwrap();
        header.set_cksum();
        builder
            .append_data(&mut header, path, Cursor::new(Vec::new()))
            .unwrap();
        builder.finish().unwrap();
        builder.into_inner().unwrap()
    }

    fn write_archive_fixture(temp: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = temp.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn resolved_archive(url: &str, sha256: &str) -> ResolvedArchive {
        ResolvedArchive {
            url: url.to_string(),
            sha256: normalize_sha256(sha256).unwrap(),
            format: ArchiveFormat::TarGz,
            strip_components: 1,
            to: "/system/foo".to_string(),
        }
    }

    fn expanded_with_archive(archive: ResolvedArchive) -> ExpandedManifest {
        ExpandedManifest {
            project_dir: PathBuf::from("/tmp/cargo-scarlet-test"),
            manifest: ScarletManifest {
                schema_version: 2,
                project: ManifestProject {
                    name: "test".to_string(),
                },
                bsp: None,
                kernel: None,
                modules: BTreeMap::new(),
                images: BTreeMap::new(),
                runner: None,
            },
            sections: BTreeMap::from([(
                "rootfs".to_string(),
                ResolvedSection {
                    layers: vec![ResolvedLayer::Archive(archive)],
                },
            )]),
        }
    }

    fn archive_lock(archive: &ResolvedArchive) -> LayerLock {
        LayerLock::Archive {
            source: LockPackageSource::archive(archive.url.clone(), archive.sha256.clone()),
            to: archive.to.clone(),
            format: archive.format.clone(),
            strip_components: archive.strip_components,
            hash: archive.sha256.clone(),
        }
    }

    fn lock_with_archive(archive: &ResolvedArchive) -> ImageLock {
        ImageLock {
            sections: BTreeMap::from([(
                "rootfs".to_string(),
                SectionLock {
                    hash: String::new(),
                    layers: vec![archive_lock(archive)],
                    files: Vec::new(),
                    packages: Vec::new(),
                },
            )]),
        }
    }

    #[test]
    fn test_normalize_sha256_accepts_bare_hex() {
        let hex = "ab".repeat(32);
        assert_eq!(normalize_sha256(&hex).unwrap(), format!("sha256:{hex}"));
    }

    #[test]
    fn test_normalize_sha256_accepts_prefixed() {
        let hex = "cd".repeat(32);
        assert_eq!(
            normalize_sha256(&format!("sha256:{hex}")).unwrap(),
            format!("sha256:{hex}")
        );
    }

    #[test]
    fn test_normalize_sha256_lowercases() {
        let hex = "AB".repeat(32);
        assert_eq!(
            normalize_sha256(&hex).unwrap(),
            format!("sha256:{}", hex.to_ascii_lowercase())
        );
    }

    #[test]
    fn test_normalize_sha256_rejects_bad_length() {
        assert!(normalize_sha256("abc").is_err());
    }

    #[test]
    fn test_archive_format_serde_kebab_case() {
        let layer: ManifestLayer = toml::from_str(
            r#"
kind = "archive"
url = "https://example.com/rootfs.tar.zst"
sha256 = "aa"
format = "tar-zst"
to = "/system"
"#,
        )
        .unwrap();
        assert!(matches!(
            layer,
            ManifestLayer::Archive {
                format: ArchiveFormat::TarZst,
                ..
            }
        ));
        let serialized = toml::to_string(&layer).unwrap();
        assert!(serialized.contains("format = \"tar-zst\""));
    }

    #[test]
    fn test_archive_layer_accepts_single_sha256() {
        let hash = "aa".repeat(32);
        let layer: ManifestLayer = toml::from_str(&format!(
            r#"
kind = "archive"
url = "https://example.com/rootfs.tar.zst"
sha256 = "sha256:{hash}"
format = "tar-zst"
to = "/system"
"#,
        ))
        .unwrap();
        let ManifestLayer::Archive { sha256, .. } = layer else {
            panic!("expected archive layer");
        };

        assert_eq!(sha256.resolve("aarch64").unwrap(), format!("sha256:{hash}"));
    }

    #[test]
    fn test_archive_layer_accepts_per_arch_sha256_map() {
        let aarch64_hash = "aa".repeat(32);
        let riscv64_hash = "bb".repeat(32);
        let layer: ManifestLayer = toml::from_str(&format!(
            r#"
kind = "archive"
url = "https://example.com/rootfs.tar.zst"
sha256 = {{ aarch64 = "sha256:{aarch64_hash}", riscv64 = "sha256:{riscv64_hash}" }}
format = "tar-zst"
to = "/system"
"#,
        ))
        .unwrap();
        let ManifestLayer::Archive { sha256, .. } = layer else {
            panic!("expected archive layer");
        };

        assert_eq!(
            sha256.resolve("aarch64").unwrap(),
            format!("sha256:{aarch64_hash}")
        );
        assert_eq!(
            sha256.resolve("riscv64").unwrap(),
            format!("sha256:{riscv64_hash}")
        );
    }

    #[test]
    fn test_archive_layer_per_arch_missing_entry_errors() {
        let sha256 = Sha256Spec::PerArch(BTreeMap::from([(
            "aarch64".to_string(),
            format!("sha256:{}", "aa".repeat(32)),
        )]));

        let error = sha256.resolve("riscv64").unwrap_err();
        assert!(error.contains("missing entry for arch 'riscv64'"));
        assert!(error.contains("have: aarch64"));
    }

    #[test]
    fn test_archive_layer_per_arch_extras_ignored() {
        let aarch64_hash = format!("sha256:{}", "aa".repeat(32));
        let sha256 = Sha256Spec::PerArch(BTreeMap::from([
            ("aarch64".to_string(), aarch64_hash.clone()),
            ("x86_64".to_string(), format!("sha256:{}", "bb".repeat(32))),
        ]));

        assert_eq!(sha256.resolve("aarch64").unwrap(), aarch64_hash);
    }

    #[test]
    fn test_archive_layer_lock_serializes_resolved_sha256() {
        let aarch64_hash = "aa".repeat(32);
        let riscv64_hash = "bb".repeat(32);
        let layers = vec![ManifestLayer::Archive {
            url: "https://example.com/rootfs-{arch}.tar.zst".to_string(),
            sha256: Sha256Spec::PerArch(BTreeMap::from([
                ("aarch64".to_string(), format!("sha256:{aarch64_hash}")),
                ("riscv64".to_string(), format!("sha256:{riscv64_hash}")),
            ])),
            format: ArchiveFormat::TarZst,
            strip_components: 0,
            to: "/system".to_string(),
        }];
        let ctx = TemplateContext {
            arch: "aarch64".to_string(),
            target_triple: "aarch64-unknown-none-elf".to_string(),
            project: "test".to_string(),
        };
        let resolved = resolve_layers(&layers, Path::new("."), &ctx, &BTreeMap::new()).unwrap();
        let [ResolvedLayer::Archive(archive)] = resolved.as_slice() else {
            panic!("expected resolved archive layer");
        };

        let lock_toml = toml::to_string(&archive_lock(archive)).unwrap();
        assert!(lock_toml.contains(&format!("sha256:{aarch64_hash}")));
        assert!(!lock_toml.contains(&riscv64_hash));
        assert!(!lock_toml.contains("riscv64"));
    }

    #[test]
    fn test_extract_rejects_absolute_path() {
        let temp = archive_test_dir("archive-absolute");
        let archive = write_archive_fixture(
            &temp,
            "archive.tar",
            &tar_bytes_with_raw_path("/etc/passwd", b"bad"),
        );
        let error = extract_archive_safe(&archive, ArchiveFormat::Tar, 0, &temp.join("dest"))
            .expect_err("absolute paths must be rejected");
        assert!(error.contains("absolute archive path"));
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_extract_rejects_dotdot_traversal() {
        let temp = archive_test_dir("archive-dotdot");
        let archive = write_archive_fixture(
            &temp,
            "archive.tar",
            &tar_bytes_with_raw_path("../../etc/passwd", b"bad"),
        );
        let error = extract_archive_safe(&archive, ArchiveFormat::Tar, 0, &temp.join("dest"))
            .expect_err("parent traversal must be rejected");
        assert!(error.contains(".."));
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_extract_rejects_symlink_escape() {
        let temp = archive_test_dir("archive-symlink-escape");
        let archive = write_archive_fixture(
            &temp,
            "archive.tar",
            &tar_link_bytes("lib", "../foo", tar::EntryType::symlink()),
        );
        let error = extract_archive_safe(&archive, ArchiveFormat::Tar, 0, &temp.join("dest"))
            .expect_err("escaping symlink must be rejected");
        assert!(error.contains("refusing symlink ../foo (escapes extraction root)"));
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_extract_allows_symlink_within_root() {
        let temp = archive_test_dir("archive-symlink-safe");
        let archive = write_archive_fixture(
            &temp,
            "archive.tar",
            &tar_link_bytes("lib", "usr/lib", tar::EntryType::symlink()),
        );
        let dest = temp.join("dest");
        extract_archive_safe(&archive, ArchiveFormat::Tar, 0, &dest).unwrap();
        assert_eq!(
            fs::read_link(dest.join("lib")).unwrap(),
            Path::new("usr/lib")
        );
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_extract_rejects_hardlink() {
        let temp = archive_test_dir("archive-hardlink");
        let archive = write_archive_fixture(
            &temp,
            "archive.tar",
            &tar_link_bytes("lib", "usr/lib", tar::EntryType::hard_link()),
        );
        let error = extract_archive_safe(&archive, ArchiveFormat::Tar, 0, &temp.join("dest"))
            .expect_err("hardlinks must be rejected");
        assert!(error.contains("refusing hardlink"));
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_extract_rejects_device_entry() {
        let temp = archive_test_dir("archive-device");
        let archive = write_archive_fixture(&temp, "archive.tar", &tar_bytes_with_raw_type(b'4'));
        let error = extract_archive_safe(&archive, ArchiveFormat::Tar, 0, &temp.join("dest"))
            .expect_err("device entries must be rejected");
        assert!(error.contains("refusing special archive entry"));
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_extract_strip_components() {
        let temp = archive_test_dir("archive-strip");
        let archive = write_archive_fixture(
            &temp,
            "archive.tar",
            &tar_bytes(&[("root/foo.txt", b"foo"), ("root/sub/bar.txt", b"bar")]),
        );
        let dest = temp.join("dest");
        extract_archive_safe(&archive, ArchiveFormat::Tar, 1, &dest).unwrap();
        assert_eq!(fs::read(dest.join("foo.txt")).unwrap(), b"foo");
        assert_eq!(fs::read(dest.join("sub/bar.txt")).unwrap(), b"bar");
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_fetch_archive_by_sha256_offline_miss() {
        let temp = archive_test_dir("archive-offline-miss");
        let expected = sha256_bytes(b"archive");
        let error = fetch_archive_by_sha256(
            "https://example.com/rootfs.tar",
            &expected,
            &temp.join("cache"),
            true,
        )
        .expect_err("offline cache miss must fail");
        assert!(error.contains("--offline: archive"));
        assert!(error.contains(&expected));
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_fetch_archive_by_sha256_cache_hit_skips_network() {
        let temp = archive_test_dir("archive-cache-hit");
        let bytes = b"cached archive";
        let expected = sha256_bytes(bytes);
        let cache = temp.join("cache");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join(expected.strip_prefix("sha256:").unwrap()), bytes).unwrap();
        let path = fetch_archive_by_sha256(
            "https://127.0.0.1:9/must-not-be-requested.tar",
            &expected,
            &cache,
            false,
        )
        .unwrap();
        assert_eq!(fs::read(path).unwrap(), bytes);
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_fetch_archive_by_sha256_rejects_sha256_mismatch() {
        let temp = archive_test_dir("archive-sha-mismatch");
        let expected = sha256_bytes(b"expected archive");
        let cache = temp.join("cache");
        fs::create_dir_all(&cache).unwrap();
        fs::write(
            cache.join(expected.strip_prefix("sha256:").unwrap()),
            b"tampered archive",
        )
        .unwrap();
        let error =
            fetch_archive_by_sha256("https://example.com/rootfs.tar", &expected, &cache, true)
                .expect_err("mismatched cached archive must fail");
        assert!(error.contains("archive SHA-256 mismatch"));
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn test_locked_drift_url_change() {
        let archive = resolved_archive("https://example.com/new-rootfs.tar.gz", &"ab".repeat(32));
        let mut lock_archive =
            resolved_archive("https://example.com/old-rootfs.tar.gz", &"ab".repeat(32));
        lock_archive.to = archive.to.clone();
        let error = validate_locked_archive_layers(
            &expanded_with_archive(archive),
            &lock_with_archive(&lock_archive),
        )
        .expect_err("changed URL must drift");
        assert!(error.contains("differs from scarlet.lock"));
    }

    #[test]
    fn test_locked_drift_sha256_change() {
        let archive = resolved_archive("https://example.com/rootfs.tar.gz", &"ab".repeat(32));
        let lock_archive = resolved_archive("https://example.com/rootfs.tar.gz", &"cd".repeat(32));
        let error = validate_locked_archive_layers(
            &expanded_with_archive(archive),
            &lock_with_archive(&lock_archive),
        )
        .expect_err("changed SHA-256 must drift");
        assert!(error.contains("differs from scarlet.lock"));
    }

    #[test]
    fn test_locked_missing_lock_entry() {
        let archive = resolved_archive("https://example.com/rootfs.tar.gz", &"ab".repeat(32));
        let error =
            validate_locked_archive_layers(&expanded_with_archive(archive), &ImageLock::default())
                .expect_err("missing archive lock must fail");
        assert!(error.contains("is missing from scarlet.lock"));
    }

    #[test]
    fn test_locked_no_drift_succeeds() {
        let archive = resolved_archive("https://example.com/rootfs.tar.gz", &"ab".repeat(32));
        validate_locked_archive_layers(
            &expanded_with_archive(resolved_archive(
                "https://example.com/rootfs.tar.gz",
                &"ab".repeat(32),
            )),
            &lock_with_archive(&archive),
        )
        .unwrap();
    }

    #[test]
    fn test_end_to_end_fake_archive_extraction() {
        use std::io::Write as _;

        let temp = archive_test_dir("archive-end-to-end");
        let tar = tar_bytes(&[("root/etc/issue", b"Scarlet\n"), ("root/bin/tool", b"tool")]);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar).unwrap();
        let compressed = encoder.finish().unwrap();
        let source = write_archive_fixture(&temp, "rootfs.tar.gz", &compressed);
        let archive = ResolvedArchive {
            url: format!("file://{}", source.display()),
            sha256: sha256_bytes(&compressed),
            format: ArchiveFormat::TarGz,
            strip_components: 1,
            to: "/system/foo".to_string(),
        };
        let staging = temp.join("staging");
        let mut locks = Vec::new();
        apply_archive_layer(&archive, &staging, &temp.join("cache"), false, &mut locks).unwrap();
        assert_eq!(
            fs::read(staging.join("system/foo/etc/issue")).unwrap(),
            b"Scarlet\n"
        );
        assert_eq!(
            fs::read(staging.join("system/foo/bin/tool")).unwrap(),
            b"tool"
        );
        assert!(matches!(locks.as_slice(), [LayerLock::Archive { .. }]));
        fs::remove_dir_all(&temp).unwrap();
    }
}
