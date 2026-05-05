set shell := ["bash", "-cu"]

pkg := justfile_directory()
bin := pkg / "dist/unity-solution-generator"
dylib := pkg / "dist/libUnitySolutionGenerator.dylib"

# List available recipes
default:
    @just --list

# Build release binary + dylib
build:
    cargo build --manifest-path "{{pkg}}/Cargo.toml" --release --workspace
    cp "{{pkg}}/target/release/unity-solution-generator" "{{bin}}"
    cp "{{pkg}}/target/release/libUnitySolutionGenerator.dylib" "{{dylib}}"
    codesign -s - -f "{{bin}}" "{{dylib}}"  # rustc strips already; re-sign so hardened runtime (Unity) accepts the binary

# Install to ~/.local/bin
install: build
    mkdir -p ~/.local/bin
    ln -sf "{{bin}}" ~/.local/bin/unity-solution-generator
    ln -sf "{{pkg}}/dist/build-unity-sln.sh" ~/.local/bin/build-unity-sln

# Deploy dylib to a Unity project's Plugins/Editor. Re-signs after copy because
# `cp` on macOS sets a `com.apple.provenance` xattr that the kernel treats as
# tampering at dlopen time (process killed by SIGKILL with no error message),
# even though `codesign -v` still passes. Adhoc re-sign clears the state.
# Usage: just deploy "$MEOW_CLIENT/Assets/Plugins/Editor/libUnitySolutionGenerator.dylib"
deploy target: build
    cp "{{dylib}}" "{{target}}"
    codesign -s - -f "{{target}}"

# Run tests
test:
    cargo test --manifest-path "{{pkg}}/Cargo.toml" --workspace

# Profile end-to-end against meow-tower (hyperfine — measures wall-clock CLI invocation)
profile: build
    #!/usr/bin/env bash
    cd "$MEOW_CLIENT"
    echo "--- generate (warm cache) ---"
    "{{bin}}" generate . ios editor > /dev/null  # warm up
    hyperfine --warmup 3 '"{{bin}}" generate . ios editor'
    echo "--- generate --root ---"
    hyperfine --warmup 3 '"{{bin}}" generate . ios editor --root'
    echo "--- lock (cold: nuke fingerprint each run) ---"
    hyperfine --warmup 1 --runs 5 \
      --prepare 'rm -f Library/UnitySolutionGenerator/lock-fingerprint' \
      '"{{bin}}" lock .'
    echo "--- lock (warm: fingerprint cache hit) ---"
    "{{bin}}" lock . > /dev/null  # ensure fingerprint exists
    hyperfine --warmup 3 '"{{bin}}" lock .'
    echo "--- startup ---"
    hyperfine --warmup 5 '"{{bin}}" --help'

# Per-section breakdown for one run of generate + lock against meow-tower
profile-spans: build
    #!/usr/bin/env bash
    cd "$MEOW_CLIENT"
    echo "--- USG_PROFILE=1 generate ---"
    USG_PROFILE=1 "{{bin}}" generate . ios editor > /dev/null
    echo
    echo "--- USG_PROFILE=1 lock (cold) ---"
    rm -f Library/UnitySolutionGenerator/lock-fingerprint
    USG_PROFILE=1 "{{bin}}" lock . > /dev/null
    echo
    echo "--- USG_PROFILE=1 lock (warm) ---"
    USG_PROFILE=1 "{{bin}}" lock . > /dev/null

# Criterion microbenchmarks (statistical, with warmup + outlier detection).
# Pass `RECIPE` to filter, e.g. `just bench scan`. Add `-- --quick` for fast runs.
bench filter='':
    cargo bench --manifest-path "{{pkg}}/Cargo.toml" {{ if filter == '' { '' } else { '--bench ' + filter } }}
