{ pkgs, rustToolchain, llvmSource, scarletAbiHash }:
let
  target = "aarch64-unknown-scarlet";
  lib = pkgs.lib;
  sources = lib.cleanSource ../.;
  rustPlatform = pkgs.makeRustPlatform {
    cargo = rustToolchain;
    rustc = rustToolchain;
  };
  cargoLock = {
    lockFile = ../Cargo.lock;
    outputHashes."scarlet-abi-1.0.0" = scarletAbiHash;
  };
  libc = rustPlatform.buildRustPackage {
    pname = "scarlet-libc";
    version = "0.1.0";
    src = sources;
    inherit cargoLock;
    buildPhase = ''
      runHook preBuild
      cargo build --locked --offline --release --features external-format --target ${target}
      runHook postBuild
    '';
    installPhase = ''
      mkdir -p "$out/lib"
      cp target/${target}/release/libscarlet_c.a "$out/lib/"
    '';
    doCheck = false;
    dontFixup = true;
  };
  clang = pkgs.llvmPackages_21.clang-unwrapped;
  llvm = pkgs.llvmPackages_21.llvm;
  lld = pkgs.llvmPackages_21.lld;
in
pkgs.stdenvNoCC.mkDerivation {
  pname = "scarlet-native-runtime";
  version = "0.1.0";
  src = sources;
  nativeBuildInputs = [ pkgs.cmake pkgs.ninja pkgs.python3 clang llvm lld ];
  dontConfigure = true;
  dontFixup = true;
  buildPhase = ''
    runHook preBuild
    cross_cc=${clang}/bin/clang
    cross_cxx=${clang}/bin/clang++
    resource=$($cross_cc -print-resource-dir)
    headers="$PWD/include"
    toolchain="$PWD/native/cmake/aarch64-scarlet.cmake"
    flags="-nostdinc -isystem $resource/include -isystem $headers -D__scarlet__=1 -fno-stack-protector -mno-outline-atomics"
    common=(
      -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_LIBDIR=lib
      -DCMAKE_POSITION_INDEPENDENT_CODE=ON
      "-DCMAKE_TOOLCHAIN_FILE=$toolchain"
      "-DCMAKE_C_COMPILER=$cross_cc" "-DCMAKE_CXX_COMPILER=$cross_cxx"
      -DCMAKE_AR=${llvm}/bin/llvm-ar -DCMAKE_RANLIB=${llvm}/bin/llvm-ranlib
      "-DCMAKE_INSTALL_PREFIX=$out"
    )
    cmake -S native -B c-support "''${common[@]}" \
      -DSCARLET_LLVM_LIBC_SOURCE_DIR=${llvmSource}/libc
    cmake --build c-support --parallel "$NIX_BUILD_CORES"
    cmake --install c-support
    cmake -S ${llvmSource}/runtimes -B cxx "''${common[@]}" \
      '-DLLVM_ENABLE_RUNTIMES=libcxx;libcxxabi' \
      -DLLVM_DEFAULT_TARGET_TRIPLE=aarch64-unknown-none-elf \
      -DLLVM_ENABLE_PER_TARGET_RUNTIME_DIR=OFF \
      -DLLVM_INCLUDE_TESTS=OFF -DLLVM_INCLUDE_DOCS=OFF \
      -DLIBCXX_INCLUDE_BENCHMARKS=OFF \
      -DLIBCXXABI_BAREMETAL=ON -DLIBCXX_ENABLE_FILESYSTEM=ON \
      -DLIBCXX_HAS_PTHREAD_API=ON -DLIBCXXABI_HAS_PTHREAD_API=ON \
      "-DCMAKE_C_FLAGS=$flags" \
      "-DCMAKE_CXX_FLAGS=$flags -nostdinc++ -D_LIBCPP_HAS_CLOCK_GETTIME" \
      -DLIBCXX_ENABLE_SHARED=OFF -DLIBCXXABI_ENABLE_SHARED=OFF \
      -DLIBCXX_ENABLE_EXCEPTIONS=OFF -DLIBCXXABI_ENABLE_EXCEPTIONS=OFF \
      -DLIBCXXABI_USE_LLVM_UNWINDER=OFF -DLIBCXXABI_USE_COMPILER_RT=OFF \
      -DLIBCXX_ENABLE_RANDOM_DEVICE=OFF -DLIBCXX_ENABLE_LOCALIZATION=ON \
      -DLIBCXX_EXTRA_SITE_DEFINES=_LIBCPP_PROVIDES_DEFAULT_RUNE_TABLE \
      -DLIBCXX_ENABLE_WIDE_CHARACTERS=OFF \
      -DLIBCXX_ENABLE_TIME_ZONE_DATABASE=OFF -DLIBCXX_HAS_TERMINAL_AVAILABLE=OFF
    cmake --build cxx --parallel "$NIX_BUILD_CORES"
    cmake --install cxx
    runHook postBuild
  '';
  installPhase = ''
    runHook preInstall
    cp ${libc}/lib/libscarlet_c.a "$out/lib/"
    mkdir -p "$out/share/licenses/LLVM"
    cp ${llvmSource}/LICENSE.TXT "$out/share/licenses/LLVM/"
    runHook postInstall
  '';
  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    resource=$(${clang}/bin/clang -print-resource-dir)
    ${clang}/bin/clang++ --target=aarch64-unknown-none-elf \
      -nostdinc -nostdinc++ -isystem "$out/include/c++/v1" \
      -isystem "$out/include" -isystem "$resource/include" \
      -D__scarlet__=1 -std=c++20 -O2 -fno-exceptions -fno-rtti \
      -fno-stack-protector -mno-outline-atomics \
      -c tests/native-runtime-link.cpp -o native-runtime-link.o
    ${clang}/bin/clang++ --target=aarch64-unknown-none-elf \
      -nostdinc -nostdinc++ -isystem "$out/include/c++/v1" \
      -isystem "$out/include" -isystem "$resource/include" \
      -D__scarlet__=1 -std=c++20 -O2 -fPIC -fvisibility=hidden \
      -fno-exceptions -fno-rtti -fno-stack-protector -mno-outline-atomics \
      -c tests/native-runtime-shared.cpp -o native-runtime-shared.o
    ${lld}/bin/ld.lld -shared --gc-sections -z max-page-size=4096 \
      --exclude-libs ALL native-runtime-shared.o --start-group \
      "$out/lib/libc++.a" "$out/lib/libc++abi.a" \
      "$out/lib/libscarlet_cxx_support.a" --end-group \
      -o libnative-runtime-shared.so
    python3 - <<'PY'
    import sys
    from pathlib import Path
    sys.path.insert(0, "tests")
    from elf_audit import Elf
    report = Elf(Path("libnative-runtime-shared.so")).report()
    assert report["machine"] == "aarch64", report
    assert not report["tls"] and not report["textrel"], report
    assert set(report["relocations"]) <= {"ABS64", "GLOB_DAT", "JUMP_SLOT", "RELATIVE"}, report
    PY
    ${lld}/bin/ld.lld -static --gc-sections --no-undefined -e _start \
      --wrap=main --undefined=__wrap_main -z max-page-size=4096 \
      ${rustToolchain}/lib/rustlib/${target}/lib/scarlet-crt0.o \
      native-runtime-link.o native-runtime-shared.o --start-group \
      "$out/lib/libc++.a" "$out/lib/libc++abi.a" \
      "$out/lib/libscarlet_c_startup.a" "$out/lib/libscarlet_cxx_support.a" \
      "$out/lib/libscarlet_c.a" --end-group -o native-runtime-link
    # Raw upstream LLD produces a generic ELF. This checks the installed
    # archive closure, not Native C startup. Native Rust consumers use the
    # Scarlet compiler's final link and OSABI handling.
    python3 tests/elf_audit.py --machine aarch64 native-runtime-link
    runHook postInstallCheck
  '';
  meta.description = "AArch64 Scarlet C/C++ runtime";
  passthru.tests.libc = rustPlatform.buildRustPackage {
    pname = "scarlet-libc-tests";
    version = "0.1.0";
    src = sources;
    inherit cargoLock;
    nativeCheckInputs = [ pkgs.python3 clang ];
    checkPhase = ''
      runHook preCheck
      cargo test --locked --offline
      python3 tests/check_headers.py --clang ${clang}/bin/clang
      runHook postCheck
    '';
    installPhase = ''mkdir -p "$out"'';
  };
}
