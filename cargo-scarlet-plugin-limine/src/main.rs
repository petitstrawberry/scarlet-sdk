use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::{Parser, ValueEnum};
use serde::Deserialize;

const DEFAULT_LIMINE_VERSION: &str = "12.4.0";

#[derive(Parser, Debug)]
#[command(name = "cargo-scarlet-plugin-limine")]
#[command(about = "Build Limine UEFI boot images for Scarlet projects")]
struct Cli {
    #[arg(long, value_enum)]
    arch: Arch,
    #[arg(long)]
    kernel: PathBuf,
    #[arg(long)]
    initramfs: PathBuf,
    #[arg(long)]
    dtb: Option<PathBuf>,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    cmdline: Option<String>,
    #[arg(long, default_value_t = 32)]
    image_slack_mb: u64,
    #[arg(long)]
    boot_image_size_mb: Option<u64>,
    #[arg(long, default_value = DEFAULT_LIMINE_VERSION)]
    limine_version: String,
    #[arg(long)]
    cache_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct PluginRequest {
    arch: Arch,
    kernel_elf: PathBuf,
    initramfs: Option<PathBuf>,
    output: PathBuf,
    section: PluginSection,
}

#[derive(Debug, Default, Deserialize)]
struct PluginSection {
    cmdline: Option<String>,
    #[serde(default)]
    dtb: Option<PathBuf>,
    #[serde(default)]
    packages: Vec<PluginPackage>,
}

#[derive(Debug, Deserialize)]
struct PluginPackage {
    source: Option<PluginSource>,
    to: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PluginSource {
    Path(String),
    Git { git: String },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Arch {
    Aarch64,
    Riscv64,
}

struct ArchSpec {
    efi_file: &'static str,
    image_label: &'static str,
    menu_name: &'static str,
    initramfs_name: &'static str,
    startup_path: &'static str,
    default_cmdline: Option<&'static str>,
    extra_config: &'static str,
}

impl Arch {
    fn name(self) -> &'static str {
        match self {
            Arch::Aarch64 => "aarch64",
            Arch::Riscv64 => "riscv64",
        }
    }

    fn spec(self) -> ArchSpec {
        match self {
            Arch::Aarch64 => ArchSpec {
                efi_file: "BOOTAA64.EFI",
                image_label: "SCARLET_AA",
                menu_name: "Scarlet AArch64",
                initramfs_name: "initramfs-aarch64.cpio",
                startup_path: "EFI\\BOOT\\BOOTAA64.EFI",
                default_cmdline: Some("console=ttyAMA0"),
                extra_config: "",
            },
            Arch::Riscv64 => ArchSpec {
                efi_file: "BOOTRISCV64.EFI",
                image_label: "SCARLET_RV",
                menu_name: "Scarlet RISC-V",
                initramfs_name: "initramfs-riscv64.cpio",
                startup_path: "EFI\\BOOT\\BOOTRISCV64.EFI",
                default_cmdline: None,
                extra_config: "    paging_mode: sv48\n",
            },
        }
    }
}

fn main() -> ExitCode {
    let result = if std::env::args_os().len() == 1 {
        read_request().and_then(|request| build_limine_image(&request.into_cli()?))
    } else {
        let cli = Cli::parse();
        build_limine_image(&cli)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cargo-scarlet-plugin-limine: error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn read_request() -> Result<PluginRequest, String> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("failed to read plugin request: {error}"))?;
    serde_json::from_str(&input).map_err(|error| format!("invalid plugin request: {error}"))
}

impl PluginRequest {
    fn into_cli(self) -> Result<Cli, String> {
        let initramfs = self
            .initramfs
            .or_else(|| self.section.initramfs_from_packages())
            .ok_or("limine plugin request does not include an initramfs")?;
        Ok(Cli {
            arch: self.arch,
            kernel: self.kernel_elf,
            initramfs,
            dtb: self.section.dtb,
            output: self.output,
            cmdline: self.section.cmdline,
            image_slack_mb: 32,
            boot_image_size_mb: None,
            limine_version: DEFAULT_LIMINE_VERSION.to_string(),
            cache_dir: None,
        })
    }
}

impl PluginSection {
    fn initramfs_from_packages(&self) -> Option<PathBuf> {
        self.packages.iter().find_map(|package| {
            if package.to == "/boot/initramfs" {
                package.source.as_ref().and_then(PluginSource::path)
            } else {
                None
            }
        })
    }
}

impl PluginSource {
    fn path(&self) -> Option<PathBuf> {
        match self {
            PluginSource::Path(path) => Some(PathBuf::from(path)),
            PluginSource::Git { git } => {
                let _ = git;
                None
            }
        }
    }
}

fn build_limine_image(cli: &Cli) -> Result<(), String> {
    require_file(&cli.kernel)?;
    require_file(&cli.initramfs)?;
    if let Some(dtb) = cli.dtb.as_deref() {
        require_file(dtb)?;
    }
    require_command("curl")?;
    require_command("git")?;
    require_command("mformat")?;
    require_command("mmd")?;
    require_command("mcopy")?;
    require_command("tar")?;

    let spec = cli.arch.spec();
    let output_parent = cli
        .output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(output_parent)
        .map_err(|error| format!("failed to create {}: {error}", output_parent.display()))?;

    let cache_dir = cli
        .cache_dir
        .clone()
        .unwrap_or_else(|| output_parent.join(".cache"));
    let limine_dir = ensure_limine_source(&cache_dir, &cli.limine_version)?;
    let boot_efi = limine_dir.join(spec.efi_file);
    require_file(&boot_efi)?;

    let work_dir = output_parent.join(".limine-work");
    fs::create_dir_all(&work_dir)
        .map_err(|error| format!("failed to create {}: {error}", work_dir.display()))?;
    let config_path = work_dir.join(format!("limine-{}.conf", cli.arch.name()));
    let cmdline = cli.cmdline.as_deref().or(spec.default_cmdline);
    let limine_config = limine_config(&spec, cmdline, cli.dtb.as_deref());
    fs::write(&config_path, limine_config)
        .map_err(|error| format!("failed to write {}: {error}", config_path.display()))?;

    let dtb_size = match cli.dtb.as_deref() {
        Some(dtb) => file_size(dtb)?,
        None => 0,
    };
    let payload_bytes = file_size(&boot_efi)?
        + file_size(&config_path)?
        + file_size(&cli.kernel)?
        + file_size(&cli.initramfs)?
        + dtb_size;
    let required_image_size_mb =
        align_up_mb(payload_bytes + cli.image_slack_mb * 1024 * 1024).max(MIN_UEFI_IMAGE_SIZE_MB);
    let image_size_mb = cli
        .boot_image_size_mb
        .filter(|size| *size >= required_image_size_mb)
        .unwrap_or(required_image_size_mb);

    let image_file = fs::File::create(&cli.output)
        .map_err(|error| format!("failed to create {}: {error}", cli.output.display()))?;
    image_file
        .set_len(image_size_mb * 1024 * 1024)
        .map_err(|error| format!("failed to size {}: {error}", cli.output.display()))?;
    drop(image_file);

    create_esp_image(&cli.output, spec.image_label)?;
    let esp_image = esp_mtools_path(&cli.output)?;
    let esp_image_str = esp_image.as_str();

    run("mmd", &["-i", esp_image_str, "::/EFI"])?;
    run("mmd", &["-i", esp_image_str, "::/EFI/BOOT"])?;
    run("mmd", &["-i", esp_image_str, "::/boot"])?;

    let efi_destination = format!("::/EFI/BOOT/{}", spec.efi_file);
    run(
        "mcopy",
        &["-i", esp_image_str, path_str(&boot_efi)?, &efi_destination],
    )?;
    run(
        "mcopy",
        &[
            "-i",
            esp_image_str,
            path_str(&config_path)?,
            "::/EFI/BOOT/limine.conf",
        ],
    )?;
    run(
        "mcopy",
        &[
            "-i",
            esp_image_str,
            path_str(&cli.kernel)?,
            "::/boot/kernel",
        ],
    )?;

    let initramfs_destination = format!("::/boot/{}", spec.initramfs_name);
    run(
        "mcopy",
        &[
            "-i",
            esp_image_str,
            path_str(&cli.initramfs)?,
            &initramfs_destination,
        ],
    )?;

    if let Some(dtb) = &cli.dtb {
        run(
            "mcopy",
            &[
                "-i",
                esp_image_str,
                path_str(dtb)?,
                "::/boot/device-tree.dtb",
            ],
        )?;
    }

    let startup = work_dir.join("startup.nsh");
    fs::write(&startup, format!("FS0:\n{}\n", spec.startup_path))
        .map_err(|error| format!("failed to write {}: {error}", startup.display()))?;
    run(
        "mcopy",
        &["-i", esp_image_str, path_str(&startup)?, "::/startup.nsh"],
    )?;

    eprintln!(
        "cargo-scarlet-plugin-limine: created {} MiB image {}",
        image_size_mb,
        cli.output.display()
    );
    Ok(())
}

const MIN_UEFI_IMAGE_SIZE_MB: u64 = 64;

fn create_esp_image(image: &Path, label: &str) -> Result<(), String> {
    run(
        "mformat",
        &["-i", path_str(image)?, "-F", "-v", label, "::"],
    )
}

fn esp_mtools_path(image: &Path) -> Result<String, String> {
    Ok(path_str(image)?.to_string())
}

fn limine_config(spec: &ArchSpec, cmdline: Option<&str>, dtb: Option<&Path>) -> String {
    let mut config = format!(
        "timeout: 0\nserial: no\nverbose: no\n\n/{}\n    protocol: limine\n    path: boot():/boot/kernel\n    module_path: boot():/boot/{}\n    module_string: initramfs\n{}",
        spec.menu_name, spec.initramfs_name, spec.extra_config
    );
    if dtb.is_some() {
        config.push_str("    dtb_path: boot():/boot/device-tree.dtb\n");
    }
    if let Some(cmdline) = cmdline {
        config.push_str("    cmdline: ");
        config.push_str(cmdline);
        config.push('\n');
    }
    config
}

fn ensure_limine_source(cache_dir: &Path, version: &str) -> Result<PathBuf, String> {
    let limine_dir = cache_dir.join(format!("limine-{version}"));
    if limine_dir.is_dir() {
        return Ok(limine_dir);
    }
    fs::create_dir_all(cache_dir)
        .map_err(|error| format!("failed to create {}: {error}", cache_dir.display()))?;

    let major = version
        .split('.')
        .next()
        .ok_or_else(|| format!("invalid Limine version: {version}"))?
        .parse::<u64>()
        .map_err(|error| format!("invalid Limine version {version}: {error}"))?;
    if major >= 12 {
        let archive = cache_dir.join(format!("limine-{version}-binary.tar.gz"));
        let url = format!(
            "https://github.com/Limine-Bootloader/Limine/releases/download/v{version}/limine-binary.tar.gz"
        );
        run(
            "curl",
            &[
                "--fail",
                "--location",
                "--output",
                path_str(&archive)?,
                &url,
            ],
        )?;
        fs::create_dir_all(&limine_dir)
            .map_err(|error| format!("failed to create {}: {error}", limine_dir.display()))?;
        run(
            "tar",
            &[
                "-xzf",
                path_str(&archive)?,
                "--strip-components=1",
                "-C",
                path_str(&limine_dir)?,
            ],
        )?;
        fs::remove_file(&archive)
            .map_err(|error| format!("failed to remove {}: {error}", archive.display()))?;
        return Ok(limine_dir);
    }

    let branch = format!("v{version}-binary");
    let destination = path_str(&limine_dir)?.to_string();
    run(
        "git",
        &[
            "clone",
            "--depth=1",
            "--branch",
            &branch,
            "https://github.com/limine-bootloader/limine.git",
            &destination,
        ],
    )?;
    Ok(limine_dir)
}

fn require_file(path: &Path) -> Result<(), String> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!("required file not found: {}", path.display()))
    }
}

fn require_command(command: &str) -> Result<(), String> {
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {command} >/dev/null 2>&1"))
        .status()
        .map_err(|error| format!("failed to check command {command}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("required command not found: {command}"))
    }
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    eprintln!(
        "cargo-scarlet-plugin-limine: running {} {}",
        program,
        args.join(" ")
    );
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|error| format!("failed to run {program}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed with status {status}"))
    }
}

fn path_str(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn file_size(path: &Path) -> Result<u64, String> {
    Ok(fs::metadata(path)
        .map_err(|error| format!("failed to stat {}: {error}", path.display()))?
        .len())
}

fn align_up_mb(bytes: u64) -> u64 {
    let mb = bytes.div_ceil(1024 * 1024);
    mb.div_ceil(16) * 16
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Arch, Cli, Parser, PluginRequest, limine_config};

    #[test]
    fn cli_uses_vhe_capable_limine_by_default() {
        // Given: a complete CLI invocation without an explicit Limine version.
        let args = [
            "cargo-scarlet-plugin-limine",
            "--arch",
            "aarch64",
            "--kernel",
            "kernel.elf",
            "--initramfs",
            "initramfs.cpio",
            "--output",
            "scarlet.img",
        ];

        // When: clap resolves the default arguments.
        let cli = Cli::try_parse_from(args).expect("the test CLI arguments should parse");

        // Then: the selected Limine release contains AArch64 EL2/VHE boot support.
        assert_eq!(cli.limine_version, "12.4.0");
    }

    #[test]
    fn cli_parses_optional_dtb() {
        let args = [
            "cargo-scarlet-plugin-limine",
            "--arch",
            "riscv64",
            "--kernel",
            "kernel.elf",
            "--initramfs",
            "initramfs.cpio",
            "--dtb",
            "boards/full.dtb",
            "--output",
            "scarlet.img",
        ];

        let cli = Cli::try_parse_from(args).expect("the test CLI arguments should parse");

        assert_eq!(cli.dtb.as_deref(), Some(Path::new("boards/full.dtb")));
    }

    #[test]
    fn limine_config_omits_dtb_path_without_dtb() {
        let config = limine_config(&Arch::Riscv64.spec(), None, None);

        assert_eq!(
            config
                .matches("    dtb_path: boot():/boot/device-tree.dtb\n")
                .count(),
            0
        );
    }

    #[test]
    fn limine_config_includes_one_dtb_path_with_dtb() {
        let config = limine_config(
            &Arch::Riscv64.spec(),
            None,
            Some(Path::new("boards/full.dtb")),
        );

        assert_eq!(
            config
                .matches("    dtb_path: boot():/boot/device-tree.dtb\n")
                .count(),
            1
        );
    }

    #[test]
    fn old_plugin_request_without_dtb_deserializes() {
        let request: PluginRequest = serde_json::from_str(
            r#"{
                "arch": "riscv64",
                "kernel_elf": "kernel.elf",
                "initramfs": "initramfs.cpio",
                "output": "scarlet.img",
                "section": { "cmdline": null, "packages": [] }
            }"#,
        )
        .expect("the legacy plugin request should deserialize");

        assert!(request.section.dtb.is_none());
    }

    #[test]
    fn plugin_request_passes_dtb_to_cli() {
        let request: PluginRequest = serde_json::from_str(
            r#"{
                "arch": "riscv64",
                "kernel_elf": "kernel.elf",
                "initramfs": "initramfs.cpio",
                "output": "scarlet.img",
                "section": {
                    "cmdline": null,
                    "dtb": "boards/full.dtb",
                    "packages": []
                }
            }"#,
        )
        .expect("the plugin request with a DTB should deserialize");

        let cli = request
            .into_cli()
            .expect("the plugin request with an initramfs should convert to CLI options");
        assert_eq!(cli.dtb.as_deref(), Some(Path::new("boards/full.dtb")));
    }
}
