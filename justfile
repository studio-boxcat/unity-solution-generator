set shell := ["bash", "-cu"]

pkg := justfile_directory()
bin := pkg / "dist/unity-solution-generator"

# List available recipes
default:
    @just --list

# Build release binary
build:
    cargo build --manifest-path "{{pkg}}/Cargo.toml" --release
    cp "{{pkg}}/target/release/unity-solution-generator" "{{bin}}"
    codesign -s - -f "{{bin}}"  # adhoc-sign so hardened runtime tools accept the binary

# Install to ~/.local/bin
install: build
    mkdir -p ~/.local/bin
    ln -sf "{{bin}}" ~/.local/bin/unity-solution-generator

# Run tests
test:
    cargo test --manifest-path "{{pkg}}/Cargo.toml"

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

# Publish to crates.io (uses `cargo publish` — irreversible).
publish:
    cargo publish --manifest-path "{{pkg}}/crates/usg-core/Cargo.toml"
