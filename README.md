# scarlet-sdk

Build tools for [Scarlet OS](https://github.com/petitstrawberry/Scarlet).

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

## Usage

```bash
cargo scarlet build --project /path/to/project
cargo scarlet image --project /path/to/project
cargo scarlet run --project /path/to/project --release
```

See [Scarlet Build System docs](https://github.com/petitstrawberry/Scarlet/tree/main/docs/build-system) for full documentation.
