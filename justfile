set shell := ["bash", "-cu"]

pkg := justfile_directory()
bin := pkg / "dist/unity-solution-generator"

# List available recipes
default:
    @just --list

# Build release binary
build:
    swift build --package-path "{{pkg}}" -c release
    strip -o "{{bin}}" "{{pkg}}/.build/release/unity-solution-generator"

# Install to ~/.local/bin
install: build
    mkdir -p ~/.local/bin
    ln -sf "{{bin}}" ~/.local/bin/unity-solution-generator
    ln -sf "{{pkg}}/dist/build-unity-sln.sh" ~/.local/bin/build-unity-sln

# Run tests
test:
    swift test --package-path "{{pkg}}"
