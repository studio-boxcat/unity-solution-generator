set shell := ["bash", "-cu"]

pkg := justfile_directory()
bin := pkg / "dist/unity-solution-generator"

# List available recipes
default:
    @just --list

# Build release binary
build:
    cargo build --manifest-path "{{pkg}}/Cargo.toml" --release
    mkdir -p "{{pkg}}/dist"
    cp "{{pkg}}/target/release/unity-solution-generator" "{{bin}}"
    codesign -s - -f "{{bin}}"  # adhoc-sign so hardened runtime tools accept the binary

# Install to ~/.local/bin (build from source — requires Rust toolchain)
install: build
    mkdir -p ~/.local/bin
    ln -sf "{{bin}}" ~/.local/bin/unity-solution-generator

# Cut a release: bump version, commit, tag, push. CI builds + uploads binary.
# After CI succeeds, run `just publish` to push to crates.io.
# Usage: just release 0.1.1
release VERSION:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! [[ "{{VERSION}}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
      echo "error: version must be semver (e.g. 0.1.1)" >&2; exit 1
    fi
    if ! git diff-index --quiet HEAD --; then
      echo "error: working tree dirty — commit or stash first" >&2; exit 1
    fi
    sed -i '' 's/^version = ".*"/version = "{{VERSION}}"/' "{{pkg}}/Cargo.toml"
    cargo update -p unity-solution-generator --manifest-path "{{pkg}}/Cargo.toml"
    git add "{{pkg}}/Cargo.toml" "{{pkg}}/Cargo.lock"
    git commit -m "release: v{{VERSION}}"
    git tag "v{{VERSION}}"
    git push origin HEAD
    git push origin "v{{VERSION}}"

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
