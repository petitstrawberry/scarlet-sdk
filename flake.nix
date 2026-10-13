{
  description = "Scarlet SDK build tools and native C/C++ runtime";

  nixConfig = {
    extra-substituters = [ "https://scarlet-rust-toolchain.cachix.org" ];
    extra-trusted-public-keys = [
      "scarlet-rust-toolchain.cachix.org-1:p+coBExi0nNTIvWF/oM9H9/1/GhwFtqGZ2Vs+4pYl6o="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    scarlet-rust-toolchain.url = "github:petitstrawberry/scarlet-rust-nix/3c23906c35053e28d56f3aa9b1d29142535ffab7";
    llvm-src = {
      url = "github:petitstrawberry/llvm-project/5733bea91affcb26db1f9f4d7459b22c79756c13";
      flake = false;
    };
    scarlet-abi-src = {
      url = "github:petitstrawberry/Scarlet/c6a831d2120c49e3d68c2839e20374d412afd015";
      flake = false;
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      scarlet-rust-toolchain,
      llvm-src,
      scarlet-abi-src,
    }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs supportedSystems (system: f system);
      mkSystem =
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          rustToolchain = scarlet-rust-toolchain.packages.${system}.scarlet-rust-toolchain;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter =
              path: type:
              let
                name = baseNameOf path;
              in
              !(type == "directory" && (name == ".git" || name == "target"));
          };
          imageTools = [
            pkgs.coreutils
            pkgs.e2fsprogs
            pkgs.mtools
          ];
          cargo-scarlet-plugin-limine = rustPlatform.buildRustPackage {
            pname = "cargo-scarlet-plugin-limine";
            version = "0.1.0";
            inherit src;
            buildAndTestSubdir = "cargo-scarlet-plugin-limine";
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [ pkgs.makeWrapper ];
            postInstall = ''
              wrapProgram "$out/bin/cargo-scarlet-plugin-limine" \
                --prefix PATH : ${pkgs.lib.makeBinPath ([ pkgs.git ] ++ imageTools)}
            '';
          };
          cargoScarletRuntimeTools = [
            rustToolchain
            cargo-scarlet-plugin-limine
            pkgs.git
            pkgs.pkg-config
            pkgs.curl
          ]
          ++ imageTools;
          cargo-scarlet = rustPlatform.buildRustPackage {
            pname = "cargo-scarlet";
            version = "0.1.0";
            inherit src;
            buildAndTestSubdir = "cargo-scarlet";
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [ pkgs.makeWrapper ];
            nativeCheckInputs = imageTools ++ [ pkgs.curl pkgs.git ];
            postInstall = ''
              wrapProgram "$out/bin/cargo-scarlet" \
                --prefix PATH : ${pkgs.lib.makeBinPath cargoScarletRuntimeTools} \
                --set CARGO_NET_GIT_FETCH_WITH_CLI true \
                --set SCARLET_CACHED_RUST_TOOLCHAIN ${rustToolchain} \
                --set SCARLET_RUST_TOOLCHAIN ${rustToolchain}
            '';
          };
        in
        {
          packages = {
            inherit cargo-scarlet cargo-scarlet-plugin-limine;
            scarlet-native-runtime = import ./scarlet-libc/native/default.nix {
              inherit pkgs rustToolchain;
              llvmSource = llvm-src;
              scarletAbiHash = scarlet-abi-src.narHash;
            };
            default = cargo-scarlet;
          };
          devShell = pkgs.mkShell {
            packages = [
              rustToolchain
              pkgs.git
              pkgs.pkg-config
              cargo-scarlet
              cargo-scarlet-plugin-limine
            ]
            ++ imageTools;
          };
        };
    in
    {
      packages = forAllSystems (system: (mkSystem system).packages);
      checks = forAllSystems (system: {
        scarlet-libc = (mkSystem system).packages.scarlet-native-runtime.tests.libc;
      });
      devShells = forAllSystems (system: {
        default = (mkSystem system).devShell;
      });
    };
}
