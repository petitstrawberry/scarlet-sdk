# scarlet-sdk

Build tools for [Scarlet](https://github.com/petitstrawberry/Scarlet).

## Installing

```bash
cargo install --git https://github.com/petitstrawberry/scarlet-sdk
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
├── src/main.rs              # Boot entry point (TODO: implement arch_start_kernel)
├── lds/                     # Linker scripts (TODO: add yours)
├── .cargo/config.toml       # Cargo build config
└── .scarlet/scarlet-modules/ # Generated module crate
```

After scaffolding, you need to:

1. Add a linker script to `lds/`
2. Implement the boot entry in `src/main.rs` (e.g. call `scarlet::arch::riscv64::boot::limine::limine_entry()`)
3. Configure `.scarlet/scarlet-modules/.cargo/config.toml` with `target`, `build-std`, and `rustflags`

Then build and run:

```bash
cargo scarlet image --project my-board
cargo scarlet run --project my-board --release
```

## Quick Start: Creating a Module

```bash
cargo scarlet new --module my-module
```

This generates a loadable kernel module with `Cargo.toml`, `module.toml`, `build.rs`, and `src/lib.rs`.

```bash
# Build the module
cargo scarlet build --module my-module

# Enable it in your project's scarlet.toml
# [modules]
# "my-module" = { path = "../my-module", enabled = true }
```

## Commands

```bash
cargo scarlet build --project <path>              # Build kernel binary
cargo scarlet check --project <path>              # Type-check without building
cargo scarlet clippy --project <path>             # Run clippy
cargo scarlet image --project <path>              # Build kernel + compose images
cargo scarlet run --project <path> --release      # Build images and launch runner
cargo scarlet update --project <path>             # Resolve git/URL sources, write lock
cargo scarlet new --project <name> --target <triple>  # Scaffold new project
cargo scarlet new --module <name>                     # Scaffold new module
```

## Documentation

See [Scarlet Build System docs](https://github.com/petitstrawberry/Scarlet/tree/main/docs/build-system) for the full specification.
