# Unity Solution Generator

Swift CLI that regenerates `.sln` and `.csproj` files for Unity projects from `asmdef`/`asmref` layout, without requiring the Unity Editor.

## Install

```bash
just install
```

Installs `unity-solution-generator` to `~/.local/bin/` (symlink).

## Commands

| Command | Description |
|---------|-------------|
| `generate` (default) | Regenerate `.csproj`/`.sln` from templates and filesystem |
| `extract-templates` | Extract templates from current Unity-generated `.csproj`/`.sln` |
| `prepare-build` | Generate platform-variant `.csproj` copies for device build validation |

```bash
unity-solution-generator -p /path/to/unity-project
unity-solution-generator extract-templates -p /path/to/unity-project
unity-solution-generator prepare-build --ios -p /path/to/unity-project
unity-solution-generator prepare-build --android --debug -p /path/to/unity-project
```

`-p` / `--project-root` sets the Unity project root. `generate` and `extract-templates` accept `--template-root <path>` (default: `Library/UnitySolutionGenerator`) to override the template directory.

## Build validation

`prepare-build` generates platform-variant `.csproj` copies that simulate device builds. It:

- Includes only runtime projects whose `includePlatforms` matches the target (or has no platform restriction)
- Strips `UNITY_EDITOR` defines (no editor API on device)
- Strips `DEBUG`/`TRACE` defines unless `--debug` is passed
- Swaps platform defines (`UNITY_ANDROID` <-> `UNITY_IOS`)
- Removes `<ProjectReference>` entries for non-runtime and non-matching-platform projects
- Rewrites remaining project references to point at their platform-variant copies

Output is one `.csproj` path per line to stdout, for piping to `parallel`:

```bash
CACHE=Library/BuildValidation/ios-prod
mkdir -p "$CACHE"
unity-solution-generator prepare-build --ios -p . \
  | parallel dotnet build {} --no-restore -v q \
      "-p:BaseIntermediateOutputPath=$CACHE/{/.}/"
```

`BaseIntermediateOutputPath` isolates build intermediates per config — without it, platform variant builds pollute the shared `obj/` directory and break subsequent editor builds.

Full validation covers the editor build plus all platform/build-type combinations (`--ios`, `--ios --debug`, `--android`, `--android --debug`), each with its own `BaseIntermediateOutputPath`.

## How it works

1. **Templates** are Unity-generated `.csproj`/`.sln` files with placeholders (`{{SOURCE_FOLDERS}}`, `{{PROJECT_REFERENCES}}`, `{{PROJECT_ROOT}}`) replacing the dynamic parts. Everything else (DLL references, analyzers, build settings, define symbols) is preserved as-is from Unity's output.
2. **Generate** discovers projects from the templates directory, scans `Assets/` and `Packages/` for `.cs` files, resolves ownership via `asmdef`/`asmref` assembly roots, builds per-directory compile patterns, and renders the templates.
3. **Project categories** (runtime/editor/test) are inferred from `.asmdef` fields:

### Category inference

| Rule | Category |
|------|----------|
| `defineConstraints` contains `"UNITY_INCLUDE_TESTS"` | **test** |
| `includePlatforms` is exactly `["Editor"]` | **editor** |
| `defineConstraints` contains `"UNITY_EDITOR"` | **editor** |
| Everything else | **runtime** |

Platform-specific assemblies (e.g. `includePlatforms: ["iOS", "Editor"]`) are treated as **runtime**, but only included in `prepare-build` when the target platform matches. Projects without `.asmdef` files (legacy assemblies) are treated as **runtime** with no platform restriction.

### Source ownership resolution

Source files are assigned per-directory: for each directory containing `.cs` files, the generator walks upward looking for the nearest `asmdef` or `asmref` assembly root. All `.cs` files in that directory belong to that assembly. Directories with no assembly root fall back to Unity's legacy assembly rules (`Assembly-CSharp`, `Assembly-CSharp-Editor`, etc.).

### Compile patterns

Instead of listing every `.cs` file individually, the generator emits per-directory glob patterns:

```xml
<Compile Include="Assets/Game/*.cs" />
<Compile Include="Assets/Game/Feature/*.cs" />
```

Directories ending with `~` or starting with `.` are excluded from scanning.

## Unity project setup

Templates live in `Library/UnitySolutionGenerator/` (gitignored). After cloning, or after Unity upgrades / package changes, re-extract and regenerate:

```bash
unity-solution-generator extract-templates -p .
unity-solution-generator -p .
```
