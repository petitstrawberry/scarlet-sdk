# scarlet-sdk

Build tools for [Scarlet](https://github.com/petitstrawberry/Scarlet).

## Installing

```bash
cargo install --git https://github.com/petitstrawberry/scarlet-sdk cargo-scarlet
cargo install --git https://github.com/petitstrawberry/scarlet-sdk cargo-scarlet-plugin-limine
```

Or from a local clone:

```bash
git clone https://github.com/petitstrawberry/scarlet-sdk.git
cd scarlet-sdk
cargo install --path cargo-scarlet
cargo install --path cargo-scarlet-plugin-limine
```

## Tools

| Package | Binary | Description |
|---------|--------|-------------|
| `cargo-scarlet` | `cargo-scarlet` | Build system CLI — reads `scarlet.toml`, builds kernel, composes images |
| `cargo-scarlet-plugin-limine` | `cargo-scarlet-plugin-limine` | Limine UEFI boot image plugin |

## Host Dependencies

Image generation uses external filesystem and disk-image tools:

- `mke2fs`, `truncate`, `du` for `format = "ext2"` and `format = "gpt-ext2"`
- `mformat`, `mmd`, and `mcopy` for `format = "limine-uefi"`

Use `format = "ext2"` and `format = "limine-uefi"` to build partition payload images, then `format = "gpt"` to compose those payloads into a GPT disk image. `format = "gpt-ext2"` remains available for simple one-partition root images. GPT partition tables are written through the Rust `gpt` crate.

### Limine boot files

`limine-uefi` supports an optional full `dtb` path for platforms that must replace the firmware device tree. Local `copy` layers on the boot image are also copied into the FAT image at their absolute `to` path:

```toml
[[images.boot.layers]]
kind = "copy"
source = ".scarlet/board.dtbo"
to = "/boot/board.dtbo"
```

The copy source must be a regular local file. The initramfs remains a dedicated Limine payload and is not copied a second time through this mechanism.

### Bundle layers

Bundle layers expand reusable `bundle.toml` layer lists recursively. Existing local paths remain supported:

```toml
[[images.rootfs.layers]]
kind = "bundle"
path = "bundles/desktop/bundle.toml"
```

Bundles can also be fetched from a Git repository. The checkout is cached under
`.scarlet/cache/git`; use a pinned revision for reproducible builds. `subdir` is
relative to the checkout root, and `bundle` defaults to `bundle.toml`.

```toml
[[images.rootfs.layers]]
kind = "bundle"
source = { git = "https://github.com/petitstrawberry/Scarlet", rev = "<commit-sha>" }
subdir = "bundles/desktop"
```

Cargo layers may likewise select a package directory below a Git checkout:

```toml
[[layers]]
kind = "cargo"
source = { git = "https://github.com/petitstrawberry/scarlet-ui" }
subdir = "examples/widget-factory"
package = "scarlet-ui-widget-factory"
bin = "scarlet-ui-widget-factory"
to = "/system/scarlet/bin/widget_factory"
```

### Project-local caches

`cargo-scarlet` keeps build inputs and child Cargo state inside the project:

- `.scarlet/cache/git` contains Git bundle and package-source checkouts managed
  directly by `cargo-scarlet`.
- `.scarlet/cache/files` contains downloaded copy and archive layer inputs.
- `.scarlet/cache/target` contains Cargo build outputs, isolated by package
  source root.
- `.scarlet/cache/cargo-home` is the `CARGO_HOME` used by child Cargo commands
  for registry and transitive Git dependencies.

Child Cargo commands do not use the invoking user's `~/.cargo` cache.

### Network access

`cargo-scarlet` does not offer an `--offline` mode. The former option only
restricted SDK-managed source downloads; it did not prevent child Cargo
commands, image plugins, scripts, hooks, or runners from accessing the network.
Caches are still reused, and missing inputs are fetched as needed.

`--locked` remains a project-lock option, not a network restriction or a promise
to forward every Cargo flag. See the [tooling contract](docs/1.0-contract.md)
for its scope and the distinction between `scarlet.lock` and Cargo lockfiles.

### Composing a disk image

```toml
[images.disk]
format = "gpt"
output = ".scarlet/images/disk.img"
deps = ["boot", "rootfs"]

[[images.disk.partitions]]
source = "boot"
name = "SCARLET_BOOT"
type = "efi-system"

[[images.disk.partitions]]
source = "rootfs"
name = "SCARLET_ROOT"
type = "linux-filesystem"
```

## Quick Start: Creating a Project

```bash
# Scaffold a new project with a local kernel source
cargo scarlet new --project my-board --target riscv64gc-unknown-none-elf --kernel-path /path/to/kernel

# Or with a git source (defaults to github.com/petitstrawberry/Scarlet)
cargo scarlet new --project my-board --target riscv64gc-unknown-none-elf
cargo scarlet new --project my-board --target riscv64gc-unknown-none-elf --kernel-rev v0.17.0
```

This generates:

```
my-board/
├── Cargo.toml
├── build.rs
├── scarlet.toml
├── src/main.rs                           # TODO: implement arch_start_kernel
├── lds/                                  # TODO: add linker script
├── .cargo/config.toml                    # TODO: set target, build-std, runner, rustflags
└── .scarlet/scarlet-modules/             # auto-generated by cargo-scarlet — do not edit
    ├── Cargo.toml
    ├── src/lib.rs
    └── .cargo/config.toml                # initialized once; configure deliberately
```

After scaffolding, you need to:

1. Edit `.cargo/config.toml` — set the actual `build.target` JSON path (including `.json`), `unstable.build-std`, `runner`, and `rustflags` (linker script path)
2. Add a linker script to `lds/`
3. Implement the boot entry in `src/main.rs` (e.g. call `scarlet::arch::riscv64::boot::limine::limine_entry()`)

The scaffold uses the legacy `[kernel]` form with the BSP at the project root;
it does not create a preconfigured board or a `bsp/` subdirectory. Define image
layers and `[runner]` before using the image/run commands below.

Aggregation source/manifest files under `.scarlet/` are generated: change the
source manifest instead. Its `.cargo/config.toml` is initialized only when
absent, so project-specific settings are retained.

Then build and run:

```bash
cargo scarlet image --project my-board
cargo scarlet run --project my-board --release
```

## Quick Start: Creating a Loadable Scarlet Module (LSM)

```bash
cargo scarlet new --lsm my-module
```

This generates a loadable scarlet module with `Cargo.toml`, `module.toml`, `build.rs`, and `src/lib.rs`.

```bash
# Build the LSM
cargo scarlet build --lsm my-module --target /path/to/kernel/targets/riscv64gc-unknown-none-elf.json
```

For LSM builds, `--target` takes a target JSON file path.

## Commands

```bash
cargo scarlet build --project <path>              # Build kernel binary
cargo scarlet check --project <path>              # Type-check without building
cargo scarlet clippy --project <path>             # Run clippy
cargo scarlet image --project <path>              # Build kernel + compose images
cargo scarlet run --project <path> --release      # Build images and launch runner
cargo scarlet update --project <path>             # Resolve git/URL sources, write lock
cargo scarlet new --project <name> --target <triple>  # Scaffold new project
cargo scarlet new --lsm <name>                       # Scaffold new loadable scarlet module
```

## Documentation

The [tooling contract](docs/1.0-contract.md) records the current CLI, manifest,
layer, lock, plugin, and execution boundaries for the coordinated 1.0 release.

See the [Scarlet build-system guide](https://github.com/petitstrawberry/Scarlet/blob/dev/docs/build-system/README.md)
for reference-project integration and [userspace development](https://github.com/petitstrawberry/Scarlet/blob/dev/docs/userspace/README.md)
for the distinction between the SDK, normal Rust std applications, and legacy
native user libraries.
