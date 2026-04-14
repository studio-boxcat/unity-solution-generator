set shell := ["bash", "-cu"]

pkg := justfile_directory()
bin := pkg / "dist/unity-solution-generator"
dylib := pkg / "dist/libUnitySolutionGenerator.dylib"

# List available recipes
default:
    @just --list

# Build release binary + dylib
build:
    swift build --package-path "{{pkg}}" -c release
    strip -o "{{bin}}" "{{pkg}}/.build/release/unity-solution-generator"
    cp "{{pkg}}/.build/release/libUnitySolutionGenerator.dylib" "{{dylib}}"

# Install to ~/.local/bin
install: build
    mkdir -p ~/.local/bin
    ln -sf "{{bin}}" ~/.local/bin/unity-solution-generator
    ln -sf "{{pkg}}/dist/build-unity-sln.sh" ~/.local/bin/build-unity-sln

# Run tests
test:
    swift test --package-path "{{pkg}}"

# Profile against meow-tower
profile: build
    #!/usr/bin/env bash
    cd "$MEOW_CLIENT"
    echo "--- generate (warm cache) ---"
    "{{bin}}" generate . ios editor > /dev/null  # warm up
    hyperfine --warmup 3 '"{{bin}}" generate . ios editor'
    echo "--- generate --root ---"
    hyperfine --warmup 3 '"{{bin}}" generate . ios editor --root'
    echo "--- lock ---"
    hyperfine --warmup 1 --runs 5 '"{{bin}}" lock .'
    echo "--- startup ---"
    hyperfine --warmup 5 '"{{bin}}" --help'
