set shell := ["bash", "-cu"]

pkg := justfile_directory()
bin := pkg / "dist/unity-solution-generator"

# List available recipes
default:
    @just --list

# Build release binary
build:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --manifest-path "{{pkg}}/Cargo.toml" --release
    mkdir -p "{{pkg}}/dist"
    if [[ "$OSTYPE" == "msys" || "$OSTYPE" == "cygwin" || "$OSTYPE" == "win32" ]]; then
      cp "{{pkg}}/target/release/unity-solution-generator.exe" "{{bin}}.exe"
    else
      cp "{{pkg}}/target/release/unity-solution-generator" "{{bin}}"
      # adhoc-sign on macOS so hardened-runtime consumers accept the binary;
      # no-op on Linux (codesign not present → skip silently).
      command -v codesign >/dev/null && codesign -s - -f "{{bin}}" || true
    fi

# Install to ~/.local/bin (build from source — requires Rust toolchain)
install: build
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$OSTYPE" == "msys" || "$OSTYPE" == "cygwin" || "$OSTYPE" == "win32" ]]; then
      mkdir -p "$USERPROFILE/.local/bin"
      cp -f "{{bin}}.exe" "$USERPROFILE/.local/bin/unity-solution-generator.exe"
    else
      mkdir -p ~/.local/bin
      ln -sf "{{bin}}" ~/.local/bin/unity-solution-generator
    fi

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
    # Cargo.lock is gitignored for this library crate; don't try to stage it.
    git add "{{pkg}}/Cargo.toml"
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
    echo "--- typecheck (warm no-op) ---"
    "{{bin}}" typecheck . > /dev/null  # warm up
    hyperfine --warmup 3 --runs 30 '"{{bin}}" typecheck .'
    echo "--- typecheck (cold: nuke scan-cache each run) ---"
    hyperfine --warmup 1 --runs 5 \
      --prepare 'rm -f Library/UnitySolutionGenerator/scan-cache.bin' \
      '"{{bin}}" typecheck .'
    echo "--- startup ---"
    hyperfine --warmup 5 '"{{bin}}" --help'

# Per-section breakdown for one run of typecheck against meow-tower
profile-spans: build
    #!/usr/bin/env bash
    cd "$MEOW_CLIENT"
    echo "--- USG_PROFILE=1 typecheck (cold) ---"
    rm -f Library/UnitySolutionGenerator/.lock-watchman-clock
    USG_PROFILE=1 "{{bin}}" typecheck . > /dev/null
    echo
    echo "--- USG_PROFILE=1 typecheck (warm) ---"
    USG_PROFILE=1 "{{bin}}" typecheck . > /dev/null

# Criterion microbenchmarks (statistical, with warmup + outlier detection).
# Pass `RECIPE` to filter, e.g. `just bench scan`. Add `-- --quick` for fast runs.
bench filter='':
    cargo bench --manifest-path "{{pkg}}/Cargo.toml" {{ if filter == '' { '' } else { '--bench ' + filter } }}

# Publish to crates.io (uses `cargo publish` — irreversible).
publish:
    cargo publish --manifest-path "{{pkg}}/crates/usg-core/Cargo.toml"
