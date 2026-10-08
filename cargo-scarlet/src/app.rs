//! Native Scarlet app format. Build recipes stay in the source checkout.
use super::*;

#[derive(Debug, Subcommand)]
pub enum AppCommands {
    Build {
        #[arg(long, default_value = ".")]
        source: PathBuf,
        #[arg(long)]
        target: String,
        #[arg(long)]
        release: bool,
        /// A new destination ending in .app; existing output is never overwritten.
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipe {
    app: Metadata,
    build: Build,
    #[serde(default)]
    resources: Vec<Resource>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct Metadata {
    id: String,
    slug: String,
    name: String,
    exec: String,
    #[serde(default)]
    args: Vec<String>,
    icon: Option<String>,
    background: Option<String>,
    background_blur: Option<String>,
    #[serde(default)]
    terminal: bool,
    #[serde(default)]
    new_instance: bool,
    #[serde(default)]
    mime_types: Vec<String>,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum Build {
    Script {
        source: String,
    },
    Prebuilt {
        source: String,
    },
    Cargo {
        #[serde(default = "dot")]
        source: String,
        package: String,
        bin: String,
        #[serde(default)]
        features: Vec<String>,
        #[serde(rename = "default-features")]
        default_features: Option<bool>,
    },
}
fn dot() -> String {
    ".".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resource {
    source: String,
    to: String,
}

fn token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && value != "."
        && value != ".."
}
fn relative(value: &str) -> Result<&Path, String> {
    if value.is_empty() || value.split('/').any(|p| !token(p)) {
        return Err(format!(
            "app path must be a portable relative path without . or ..: {value}"
        ));
    }
    Ok(Path::new(value))
}
fn text_value(value: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().any(|c| c.is_control()) {
        return Err("empty/control character in app metadata".into());
    }
    Ok(())
}
fn slug(value: &str) -> Result<(), String> {
    if !token(value) || value.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err("app directory slug must be lowercase ASCII".into());
    }
    Ok(())
}
fn source_path(root: &Path, value: &str) -> Result<PathBuf, String> {
    let path = root.join(value);
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !canonical.starts_with(root.canonicalize().map_err(|e| e.to_string())?) {
        return Err(format!("app source escapes source directory: {value}"));
    }
    Ok(canonical)
}
fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "app symlinks are unsupported: {}",
            source.display()
        ));
    }
    if metadata.is_dir() {
        fs::create_dir_all(destination).map_err(|e| e.to_string())?;
        for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else if metadata.is_file() {
        if destination.exists() {
            return Err(format!("app file collision: {}", destination.display()));
        }
        fs::create_dir_all(destination.parent().ok_or("missing app file parent")?)
            .map_err(|e| e.to_string())?;
        fs::copy(source, destination).map_err(|e| e.to_string())?;
    } else {
        return Err("unsupported app file type".into());
    }
    Ok(())
}
fn native_machine(target: &str) -> Result<u16, String> {
    match target {
        "aarch64-unknown-scarlet" => Ok(183),
        "riscv64gc-unknown-scarlet" => Ok(243),
        _ => Err(format!("unsupported native app target: {target}")),
    }
}
fn native_elf(path: &Path, target: &str) -> Result<(), String> {
    let machine = native_machine(target)?;
    let data = fs::read(path).map_err(|e| e.to_string())?;
    if data.len() < 64
        || &data[..8] != b"\x7fELF\x02\x01\x01\x53"
        || u16::from_le_bytes([data[18], data[19]]) != machine
        || !matches!(u16::from_le_bytes([data[16], data[17]]), 2 | 3)
    {
        return Err(format!(
            "expected native Scarlet ELF64 for {target}: {}",
            path.display()
        ));
    }
    let phoff = usize::try_from(u64::from_le_bytes(data[32..40].try_into().unwrap()))
        .map_err(|_| "invalid ELF program offset")?;
    let phsize = usize::from(u16::from_le_bytes([data[54], data[55]]));
    let phnum = usize::from(u16::from_le_bytes([data[56], data[57]]));
    if phnum > 0
        && (phsize != 56
            || phoff
                .checked_add(phnum * phsize)
                .is_none_or(|end| end > data.len()))
    {
        return Err("invalid native ELF program headers".into());
    }
    let mut interpreters = 0;
    for index in 0..phnum {
        let header = &data[phoff + index * phsize..phoff + (index + 1) * phsize];
        if u32::from_le_bytes(header[..4].try_into().unwrap()) == 3 {
            interpreters += 1;
            let offset = usize::try_from(u64::from_le_bytes(header[8..16].try_into().unwrap()))
                .map_err(|_| "invalid interpreter offset")?;
            let size = usize::try_from(u64::from_le_bytes(header[32..40].try_into().unwrap()))
                .map_err(|_| "invalid interpreter size")?;
            if offset.checked_add(size).is_none_or(|end| end > data.len())
                || &data[offset..offset + size] != b"/bin/scarlet-ld\0"
            {
                return Err("native app interpreter must be /bin/scarlet-ld".into());
            }
        }
    }
    if interpreters > 1 {
        return Err("multiple native app interpreters".into());
    }
    Ok(())
}
fn exec_words(value: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for c in value.chars() {
        if c.is_control() {
            return Err("control character in app Exec".into());
        }
        if escaped {
            current.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            quoted = !quoted;
        } else if c.is_whitespace() && !quoted {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
    }
    if quoted || escaped {
        return Err("malformed app Exec".into());
    }
    if !current.is_empty() {
        words.push(current);
    }
    if words.iter().any(|word| {
        word.contains('%') && !matches!(word.as_str(), "%f" | "%F" | "%u" | "%U" | "%%")
    }) {
        return Err("unsupported app Exec field code".into());
    }
    Ok(words)
}
fn descriptor(metadata: &Metadata, target: &str) -> Result<String, String> {
    if !token(&metadata.id) {
        return Err("invalid stable app ID".into());
    }
    slug(&metadata.slug)?;
    relative(&metadata.exec)?;
    text_value(&metadata.name)?;
    let mut out = format!(
        "[Desktop Entry]\nType=Application\nName={}\nExec={}",
        metadata.name, metadata.exec
    );
    for arg in &metadata.args {
        text_value(arg)?;
        if arg.contains('%') && !matches!(arg.as_str(), "%f" | "%F" | "%u" | "%U" | "%%") {
            return Err("unsupported app Exec field code".into());
        }
        write!(
            out,
            " \"{}\"",
            arg.replace('\\', "\\\\").replace('"', "\\\"")
        )
        .unwrap();
    }
    write!(out, "\nTerminal={}\nX-Scarlet-NewInstance={}\nX-Scarlet-AppFormat=1\nX-Scarlet-Target={target}\n", metadata.terminal, metadata.new_instance).unwrap();
    for (key, value) in [
        ("Icon", &metadata.icon),
        ("X-Scarlet-Background", &metadata.background),
        ("X-Scarlet-BackgroundBlur", &metadata.background_blur),
    ] {
        if let Some(value) = value {
            text_value(value)?;
            if key == "X-Scarlet-Background" || (key == "Icon" && value.contains('/')) {
                relative(value)?;
            }
            if key == "X-Scarlet-BackgroundBlur"
                && !matches!(value.as_str(), "none" | "full" | "label")
            {
                return Err("invalid background blur".into());
            }
            writeln!(out, "{key}={value}").unwrap();
        }
    }
    if !metadata.mime_types.is_empty() {
        for mime in &metadata.mime_types {
            text_value(mime)?;
            if mime.contains(';') || !mime.contains('/') {
                return Err("invalid MIME type".into());
            }
        }
        writeln!(out, "MimeType={};", metadata.mime_types.join(";")).unwrap();
    }
    Ok(out)
}
fn inspect_tree(
    root: &Path,
    path: &Path,
    desktops: &mut Vec<PathBuf>,
    target: &str,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() {
        return Err("app symlinks are unsupported".into());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
            inspect_tree(
                root,
                &entry.map_err(|e| e.to_string())?.path(),
                desktops,
                target,
            )?;
        }
    } else if metadata.is_file() {
        let rel = path.strip_prefix(root).map_err(|e| e.to_string())?;
        relative(rel.to_str().ok_or("non UTF-8 app path")?)?;
        if matches!(
            path.file_name().and_then(|s| s.to_str()),
            Some("app.toml" | "Cargo.toml" | "bundle.toml")
        ) {
            return Err("build recipes must not be shipped in an app".into());
        }
        if path.extension().is_some_and(|v| v == "desktop") {
            desktops.push(path.to_path_buf());
        }
        let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
        let mut magic = [0; 4];
        if file.read(&mut magic).map_err(|e| e.to_string())? == 4 && &magic == b"\x7fELF" {
            native_elf(path, target)?;
        }
    } else {
        return Err("unsupported app file type".into());
    }
    Ok(())
}

/// Validate finished app artifacts before either CLI publication or image staging.
pub fn validate(root: &Path, target: &str) -> Result<(), String> {
    let name = root
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("invalid app directory")?;
    slug(
        name.strip_suffix(".app")
            .ok_or("app directory must end in .app")?,
    )?;
    let mut desktops = Vec::new();
    inspect_tree(root, root, &mut desktops, target)?;
    if desktops.len() != 1 || desktops[0].parent() != Some(root) {
        return Err("app requires exactly one root desktop descriptor".into());
    }
    let id = desktops[0]
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("invalid app ID")?;
    if !token(id) {
        return Err("invalid stable app ID".into());
    }
    let content = fs::read_to_string(&desktops[0]).map_err(|e| e.to_string())?;
    let mut fields = BTreeMap::new();
    let mut section = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line == "[Desktop Entry]";
            continue;
        }
        if section && !line.is_empty() && !line.starts_with('#') {
            let (key, value) = line.split_once('=').ok_or("invalid app descriptor")?;
            if fields.insert(key, value).is_some() {
                return Err("duplicate app descriptor field".into());
            }
        }
    }
    if fields.get("X-Scarlet-AppFormat") != Some(&"1")
        || fields.get("X-Scarlet-Target") != Some(&target)
        || fields.get("Type") != Some(&"Application")
    {
        return Err("unsupported app format/target/type".into());
    }
    text_value(fields.get("Name").ok_or("missing app name")?)?;
    let exec = fields.get("Exec").ok_or("missing app executable")?;
    let words = exec_words(exec)?;
    let executable = words.first().ok_or("missing app executable")?;
    relative(executable)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(root.join(executable))
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o111
            == 0
        {
            return Err("app executable lacks executable permissions".into());
        }
    }
    native_elf(&root.join(executable), target)?;
    for key in ["Icon", "X-Scarlet-Background"] {
        if let Some(value) = fields.get(key) {
            if key == "X-Scarlet-Background" || value.contains('/') {
                relative(value)?;
                if !root.join(value).is_file() {
                    return Err(format!("missing app resource: {value}"));
                }
            } else if !token(value) {
                return Err("invalid themed app icon".into());
            }
        }
    }
    Ok(())
}

/// Build source+app.toml or copy a validated prebuilt .app into a new output.
fn build_inner(
    source: &Path,
    output: &Path,
    target: &str,
    release: bool,
    project: &Path,
) -> Result<(), String> {
    if fs::symlink_metadata(output).is_ok() {
        return Err(format!("app output already exists: {}", output.display()));
    }
    let source = source.canonicalize().map_err(|e| e.to_string())?;
    let output = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(output)
    };
    if source.extension().is_some_and(|v| v == "app") {
        validate(&source, target)?;
        copy_tree(&source, &output)?;
    } else {
        let recipe_path = if source.is_dir() {
            source.join("app.toml")
        } else {
            source.clone()
        };
        if recipe_path.file_name().and_then(|s| s.to_str()) != Some("app.toml") {
            return Err("app source recipe must be named app.toml".into());
        }
        let root = recipe_path.parent().ok_or("missing recipe directory")?;
        let recipe: Recipe =
            toml::from_str(&fs::read_to_string(&recipe_path).map_err(|e| e.to_string())?)
                .map_err(|e| format!("invalid app.toml: {e}"))?;
        let desktop = descriptor(&recipe.app, target)?;
        if output.file_name().and_then(|v| v.to_str()) != Some(&format!("{}.app", recipe.app.slug))
        {
            return Err("output directory must match the app slug".into());
        }
        let destination = output.join(relative(&recipe.app.exec)?);
        fs::create_dir_all(destination.parent().ok_or("missing executable parent")?)
            .map_err(|e| e.to_string())?;
        match &recipe.build {
            Build::Script { source } => {
                let script = source_path(root, source)?;
                let status = Command::new(script)
                    .arg(target)
                    .arg(&destination)
                    .current_dir(root)
                    .status()
                    .map_err(|e| e.to_string())?;
                if !status.success() {
                    return Err("app build script failed".into());
                }
            }
            Build::Prebuilt { source } => {
                copy_tree(&source_path(root, source)?, &destination)?;
            }
            Build::Cargo {
                source,
                package,
                bin,
                features,
                default_features,
            } => {
                let pkg = ResolvedPackage {
                    kind: Some("cargo".into()),
                    source: Some(PackageSource::Path(source.clone())),
                    local_source: Some(source_path(root, source)?),
                    resolved_rev: None,
                    package_name: Some(package.clone()),
                    bin: Some(bin.clone()),
                    subdir: None,
                    features: features.clone(),
                    default_features: *default_features,
                    from: None,
                    to: recipe.app.exec.clone(),
                    output: None,
                };
                let cargo_project = if project.join("scarlet.toml").is_file() {
                    project
                } else {
                    root
                };
                install_package(
                    &output,
                    &pkg,
                    cargo_project,
                    target,
                    if release { "release" } else { "debug" },
                    None,
                )?;
            }
        }
        for resource in &recipe.resources {
            copy_tree(
                &source_path(root, &resource.source)?,
                &output.join(relative(&resource.to)?),
            )?;
        }
        let descriptor = output.join(format!("{}.desktop", recipe.app.id));
        if descriptor.exists() {
            return Err("resource collides with generated app descriptor".into());
        }
        fs::write(descriptor, desktop).map_err(|e| e.to_string())?;
    }
    validate(&output, target)
}

pub fn build(
    source: &Path,
    output: &Path,
    target: &str,
    release: bool,
    project: &Path,
) -> Result<(), String> {
    native_machine(target)?;
    let source = source.canonicalize().map_err(|e| e.to_string())?;
    // App builds can share project Cargo caches and stage inside .scarlet.
    // Retain the guard through child execution and final output publication.
    // Standalone recipes use their source directory as the Cargo cache root.
    let cache_project = if project.join("scarlet.toml").is_file() {
        project
    } else if source.is_dir() {
        source.as_path()
    } else {
        source.parent().ok_or("missing recipe directory")?
    };
    let _project_lock = if cache_project.join("scarlet.toml").is_file() {
        Some(cache::ProjectLock::activity(cache_project)?)
    } else {
        None
    };
    let absolute = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(output)
    };
    let name = absolute.file_name().ok_or("invalid app output")?;
    let parent = absolute.parent().ok_or("invalid app output")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let parent = parent.canonicalize().map_err(|e| e.to_string())?;
    let destination = parent.join(name);
    if fs::symlink_metadata(&destination).is_ok() {
        return Err("app output already exists".into());
    }
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let temporary = parent.join(format!(
        ".scarlet-app-build-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::create_dir(&temporary).map_err(|e| e.to_string())?;
    let staged = temporary.join(name);
    let result = build_inner(&source, &staged, target, release, project).and_then(|()| {
        if fs::symlink_metadata(&destination).is_ok() {
            return Err("app output appeared during build".into());
        }
        fs::rename(&staged, &destination).map_err(|e| e.to_string())
    });
    let cleanup = fs::remove_dir_all(&temporary).map_err(|e| e.to_string());
    result.and(cleanup)
}

pub fn run(command: AppCommands) -> Result<(), String> {
    match command {
        AppCommands::Build {
            source,
            target,
            release,
            output,
        } => {
            let project = std::env::current_dir().map_err(|e| e.to_string())?;
            build(&source, &output, &target, release, &project)?;
            eprintln!("cargo-scarlet: app ready at {}", output.display());
            Ok(())
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "scarlet-app-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(root.join("source")).unwrap();
            let mut elf = [0u8; 64];
            elf[..8].copy_from_slice(b"\x7fELF\x02\x01\x01\x53");
            elf[16..18].copy_from_slice(&2u16.to_le_bytes());
            elf[18..20].copy_from_slice(&183u16.to_le_bytes());
            fs::write(root.join("source/player"), elf).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    root.join("source/player"),
                    fs::Permissions::from_mode(0o755),
                )
                .unwrap();
            }
            fs::write(root.join("source/art.png"), "art fixture").unwrap();
            fs::write(root.join("source/app.toml"), Self::recipe("bin/player")).unwrap();
            Self(root)
        }
        fn recipe(exec: &str) -> String {
            format!(
                r#"[app]
id = "org.test.player"
slug = "player"
name = "Player"
exec = "{exec}"
args = ["%F", "--label", "a b"]
icon = "resources/art.png"
mime-types = ["audio/wav"]
[build]
kind = "prebuilt"
source = "player"
[[resources]]
source = "art.png"
to = "resources/art.png"
"#
            )
        }
        fn build(&self) -> Result<(), String> {
            build(
                &self.0.join("source"),
                &self.0.join("player.app"),
                "aarch64-unknown-scarlet",
                false,
                &self.0,
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn source_and_prebuilt_use_identical_validated_artifacts() {
        let f = Fixture::new();
        f.build().unwrap();
        let built = f.0.join("player.app");
        assert!(!built.join("app.toml").exists());
        let text = fs::read_to_string(built.join("org.test.player.desktop")).unwrap();
        assert!(text.contains("Exec=bin/player \"%F\" \"--label\" \"a b\""));
        let renamed = f.0.join("renamed.app");
        build(&built, &renamed, "aarch64-unknown-scarlet", false, &f.0).unwrap();
        assert_eq!(sha256_dir(&built).unwrap(), sha256_dir(&renamed).unwrap());
    }
    #[test]
    fn failed_build_is_not_published_and_existing_output_is_preserved() {
        let f = Fixture::new();
        fs::write(f.0.join("source/app.toml"), Fixture::recipe("../escape")).unwrap();
        assert!(f.build().is_err());
        assert!(!f.0.join("player.app").exists());
        fs::write(f.0.join("source/app.toml"), Fixture::recipe("bin/player")).unwrap();
        f.build().unwrap();
        let original = sha256_dir(&f.0.join("player.app")).unwrap();
        assert!(f.build().is_err());
        assert_eq!(original, sha256_dir(&f.0.join("player.app")).unwrap());
    }
    #[test]
    fn rejects_traversal_metadata_injection_and_mixed_abi_target() {
        let f = Fixture::new();
        for path in [
            "/bin/player",
            "bin/../player",
            "../player",
            "bin//player",
            "bin/./player",
        ] {
            fs::write(f.0.join("source/app.toml"), Fixture::recipe(path)).unwrap();
            assert!(f.build().is_err(), "{path}");
        }
        let recipe = Fixture::recipe("bin/player");
        for broken in [
            recipe.replace("org.test.player", "../bad"),
            recipe.replace("slug = \"player\"", "slug = \"Player\""),
            recipe.replace("a b", "a\\nExec=/bin/sh"),
            recipe.replace("%F", "%z"),
        ] {
            fs::write(f.0.join("source/app.toml"), broken).unwrap();
            assert!(f.build().is_err());
        }
        fs::write(f.0.join("source/app.toml"), recipe).unwrap();
        let binary = f.0.join("source/player");
        let mut data = fs::read(&binary).unwrap();
        data[7] = 3;
        fs::write(binary, data).unwrap();
        assert!(f.build().is_err());
    }
    #[test]
    fn rejects_ambiguous_descriptors_recipes_and_wrong_target_prebuilt() {
        let f = Fixture::new();
        f.build().unwrap();
        let app = f.0.join("player.app");
        assert!(validate(&app, "riscv64gc-unknown-scarlet").is_err());
        fs::write(app.join("other.desktop"), "fixture").unwrap();
        assert!(validate(&app, "aarch64-unknown-scarlet").is_err());
        fs::remove_file(app.join("other.desktop")).unwrap();
        fs::write(app.join("app.toml"), "fixture").unwrap();
        assert!(validate(&app, "aarch64-unknown-scarlet").is_err());
    }
    #[cfg(unix)]
    #[test]
    fn rejects_symlink_resource_escapes_and_nonexecutable_entrypoint() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let f = Fixture::new();
        f.build().unwrap();
        let app = f.0.join("player.app");
        let art = app.join("resources/art.png");
        fs::remove_file(&art).unwrap();
        symlink("/etc/passwd", &art).unwrap();
        assert!(validate(&app, "aarch64-unknown-scarlet").is_err());
        fs::remove_file(&art).unwrap();
        fs::write(&art, "fixture").unwrap();
        fs::set_permissions(app.join("bin/player"), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(validate(&app, "aarch64-unknown-scarlet").is_err());
    }
    #[test]
    fn app_layer_builds_source_then_prebuilt_into_clean_newc_staging() {
        let f = Fixture::new();
        fs::create_dir_all(f.0.join("bsp/.cargo")).unwrap();
        fs::write(
            f.0.join("bsp/.cargo/config.toml"),
            "[build]\ntarget = \"aarch64-unknown-none\"\n",
        )
        .unwrap();
        fs::write(
            f.0.join("scarlet.toml"),
            r#"schema_version = 2
[project]
name = "app-test"
[bsp]
path = "bsp"
package = "scarlet"
[bsp.kernel]
source = { path = "kernel" }
[images.rootfs]
format = "newc"
output = ".scarlet/images/rootfs.cpio"
[[images.rootfs.layers]]
kind = "app"
source = "source"
to = "/applications/player.app"
"#,
        )
        .unwrap();
        build_manifest_image(
            &f.0,
            None,
            false,
            Some(f.0.join("unused-kernel")),
            true,
            false,
            &[],
        )
        .unwrap();
        let staged = f.0.join(".scarlet/staging/rootfs/applications/player.app");
        validate(&staged, "aarch64-unknown-scarlet").unwrap();
        let hash = sha256_dir(&staged).unwrap();
        let lock = load_lock(&f.0);
        assert!(
            matches!(&lock.sections["rootfs"].layers[0], LayerLock::App { hash: value, .. } if value == &hash)
        );
        assert!(!f.0.join(".scarlet/staging/rootfs/system").exists());
        let app = f.0.join("player.app");
        copy_tree(&staged, &app).unwrap();
        let manifest = fs::read_to_string(f.0.join("scarlet.toml"))
            .unwrap()
            .replace("source = \"source\"", "source = \"player.app\"");
        fs::write(f.0.join("scarlet.toml"), manifest).unwrap();
        assert!(
            build_manifest_image(
                &f.0,
                None,
                false,
                Some(f.0.join("unused-kernel")),
                true,
                true,
                &[]
            )
            .unwrap_err()
            .contains("--locked: app layer")
        );
        build_manifest_image(
            &f.0,
            None,
            false,
            Some(f.0.join("unused-kernel")),
            true,
            false,
            &[],
        )
        .unwrap();
        assert_eq!(sha256_dir(&staged).unwrap(), hash);
    }
    #[test]
    fn app_cli_and_cargo_recipe_are_first_class() {
        let cli = Cli::try_parse_from([
            "cargo-scarlet",
            "app",
            "build",
            "--source",
            "src",
            "--target",
            "aarch64-unknown-scarlet",
            "--output",
            "player.app",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::App {
                command: AppCommands::Build { .. }
            }
        ));
        let recipe = Fixture::recipe("bin/player").replace("kind = \"prebuilt\"\nsource = \"player\"", "kind = \"cargo\"\npackage = \"player\"\nbin = \"player\"\nfeatures = [\"native\"]\ndefault-features = false");
        let recipe: Recipe = toml::from_str(&recipe).unwrap();
        assert!(matches!(
            recipe.build,
            Build::Cargo {
                default_features: Some(false),
                ..
            }
        ));
    }
}
