set shell := ["bash", "-cu"]

pkg := justfile_directory()
bin := pkg / ".build/release/unity-solution-generator"

# List available recipes
default:
    @just --list

# Build release binary
build:
    swift build --package-path "{{pkg}}" -c release
    strip "{{bin}}"

# Install to ~/.local/bin
install: build
    mkdir -p ~/.local/bin
    ln -sf "{{bin}}" ~/.local/bin/unity-solution-generator

# Run tests
test:
    swift test --package-path "{{pkg}}"
