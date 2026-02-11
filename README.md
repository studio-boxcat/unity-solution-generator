# Unity Solution Generator

Swift CLI that regenerates `.csproj` and `.sln` files for Unity projects from `asmdef`/`asmref` layout, without requiring the Unity Editor.

## Install

```bash
just install
```

Installs `unity-solution-generator` to `~/.local/bin/` (symlink).

## Commands

| Command | Description |
|---------|-------------|
| `generate` (default) | Regenerate `.csproj`/`.sln` from templates and filesystem |
| `extract-templates` | Extract `.csproj` templates from Unity-generated project files |

```bash
unity-solution-generator -p .                     # scan only (show stats)
unity-solution-generator --ios -p .               # ios-prod
unity-solution-generator --ios --debug -p .        # ios-dev
unity-solution-generator --ios --editor -p .       # ios-editor
unity-solution-generator --android -p .            # android-prod
unity-solution-generator extract-templates -p .
```

`-p` / `--project-root` sets the Unity project root. Both commands accept `--generator-root <path>` (default: `Library/UnitySolutionGenerator`).

### Platform + configuration

Two orthogonal axes: **platform** (`--ios`, `--android`) and **configuration** (`--editor`, `--debug`, default `prod`).

| Config | Projects | UNITY_EDITOR | DEBUG/TRACE |
|--------|----------|--------------|-------------|
| `prod` (default) | runtime only | stripped | stripped |
| `dev` (`--debug`) | runtime only | stripped | kept |
| `editor` (`--editor`) | all | kept | kept |

All configs swap platform defines (`UNITY_ANDROID` <-> `UNITY_IOS`) to match the target. Prod/dev also strip `<ProjectReference>` entries for excluded projects.

Each invocation produces one variant in `{platform}-{config}/` containing `.csproj` files and a `.sln`. Generated files use relative paths (`../../../Assets/...`) to reach the project root.

## Directory structure

All generator artifacts live under `Library/UnitySolutionGenerator/` (gitignored):

```
Library/UnitySolutionGenerator/
  templates/                    ← extracted from Unity-generated .csproj files
    MyProject.csproj.template
  ios-prod/                     ← each variant: .csproj files + .sln
  ios-dev/
  ios-editor/
  android-prod/
  ...
```

## Build validation

Output is the `.sln` path to stdout, for use with `dotnet build`:

```bash
dotnet build "$(unity-solution-generator --ios -p .)" -m --no-restore -v q
```

Full validation covers all 6 platform/config combinations:

```bash
for flags in "--ios" "--ios --debug" "--ios --editor" \
             "--android" "--android --debug" "--android --editor"; do
  dotnet build "$(unity-solution-generator $flags -p .)" -m --no-restore -v q
done
```

## How it works

1. **Templates** are Unity-generated `.csproj` files with placeholders (`{{SOURCE_FOLDERS}}`, `{{PROJECT_REFERENCES}}`, `{{PROJECT_ROOT}}`) replacing the dynamic parts. Everything else (DLL references, analyzers, build settings, define symbols) is preserved as-is from Unity's output. The `.sln` is generated from a hardcoded minimal template.
2. **Generate** discovers projects from the templates directory, scans `Assets/` and `Packages/` for directories containing `.cs` files, resolves ownership via `asmdef`/`asmref` assembly roots, builds per-directory compile patterns, and renders the templates into a variant subdirectory.
3. **Project categories** (runtime/editor/test) are inferred from `.asmdef` fields:

### Category inference

| Rule | Category |
|------|----------|
| `defineConstraints` contains `"UNITY_INCLUDE_TESTS"` | **test** |
| `includePlatforms` is exactly `["Editor"]` | **editor** |
| `defineConstraints` contains `"UNITY_EDITOR"` | **editor** |
| Everything else | **runtime** |

Platform-specific assemblies (e.g. `includePlatforms: ["iOS", "Editor"]`) are treated as **runtime**, but only included in prod/dev variants when the target platform matches. Editor variants include all projects regardless. Projects without `.asmdef` files (legacy assemblies) are treated as **runtime** with no platform restriction.

### Source ownership resolution

Source files are assigned per-directory: for each directory containing `.cs` files, the generator walks upward looking for the nearest `asmdef` or `asmref` assembly root. All `.cs` files in that directory belong to that assembly. Directories with no assembly root fall back to Unity's legacy assembly rules (`Assembly-CSharp`, `Assembly-CSharp-Editor`, etc.).

### Compile patterns

Instead of listing every `.cs` file individually, the generator emits per-directory relative glob patterns:

```xml
<Compile Include="../../../Assets/Game/*.cs" />
<Compile Include="../../../Assets/Game/Feature/*.cs" />
```

Directories ending with `~` or starting with `.` are excluded from scanning.

## Unity project setup

After cloning, or after Unity upgrades / package changes, re-extract templates:

```bash
unity-solution-generator extract-templates -p .
```
