# Unity Solution Generator

Swift CLI that regenerates `.csproj` and `.sln` files for Unity projects from `asmdef`/`asmref` layout, without requiring the Unity Editor.

## Install

```bash
just install
```

Installs to `~/.local/bin/` (symlinks):
- `unity-solution-generator` — the generator binary
- `build-unity-sln` — build script with optimized MSBuild args

## Commands

| Command | Description |
|---------|-------------|
| `init` | Extract `.csproj` templates from Unity-generated project files |
| `generate` | Regenerate `.csproj`/`.sln` from templates and filesystem |

```bash
unity-solution-generator init .                           # extract templates
unity-solution-generator generate . ios prod              # ios-prod
unity-solution-generator generate . ios dev               # ios-dev
unity-solution-generator generate . ios editor            # ios-editor
unity-solution-generator generate . android prod          # android-prod
```

Positional args: `<command> <unity-root> <platform> <config>`.

### Platform + configuration

Two orthogonal axes: **platform** (`ios`, `android`) and **configuration** (`prod`, `dev`, `editor`).

| Config | Projects | DefineConstants (via Directory.Build.props) |
|--------|----------|---------------------------------------------|
| `prod` | runtime only | platform defines only |
| `dev` | runtime only | platform + `DEBUG;TRACE;UNITY_ASSERTIONS` |
| `editor` | all | platform + `UNITY_EDITOR;UNITY_EDITOR_64;UNITY_EDITOR_OSX;DEBUG;TRACE;UNITY_ASSERTIONS` |

Dynamic defines are injected via `Directory.Build.props` per variant — templates contain only static defines with a `$(DefineConstants)` reference. Prod/dev variants exclude `<ProjectReference>` entries for editor/test projects during rendering.

Each invocation produces one variant in `{platform}-{config}/` containing `.csproj` files, a `.sln`, and a `Directory.Build.props`.

## Directory structure

All generator artifacts live under `Library/UnitySolutionGenerator/` (gitignored):

```
Library/UnitySolutionGenerator/
  templates/                    ← extracted from Unity-generated .csproj files
    MyProject.csproj.template
  ios-prod/                     ← variant: .csproj + .sln + Directory.Build.props
  ios-dev/
  ios-editor/
  android-prod/
  ...
```

## Build validation

`build-unity-sln.sh` wraps `unity-solution-generator generate` + `dotnet build` with optimized MSBuild args (quiet output, RAR skip, shared compilation). Run from a Unity project root:

```bash
build-unity-sln.sh ios prod          # build ios-prod variant
build-unity-sln.sh android dev       # build android-dev variant
build-unity-sln.sh --clean           # clean cached artifacts
```

Or call `unity-solution-generator` directly — output is the `.sln` path to stdout:

```bash
dotnet build "$(unity-solution-generator generate . ios prod)" -m --no-restore -v q
```

Full validation covers all 6 platform/config combinations:

```bash
for p in ios android; do for c in prod dev editor; do
  dotnet build "$(unity-solution-generator generate . $p $c)" -m --no-restore -v q
done; done
```

## How it works

1. **Init** reads Unity-generated `.csproj` files and strips dynamic parts: `<Compile>`, `<ProjectReference>`, dynamic defines, and `</Project>` are removed. Absolute paths become `$(ProjectRoot)`, dynamic defines become `$(DefineConstants)`. Everything else (DLL references, analyzers, build settings) is preserved as-is.
2. **Generate** discovers projects from the templates directory, scans `Assets/` and `Packages/` for directories containing `.cs` files, resolves ownership via `asmdef`/`asmref` assembly roots, and appends `<ItemGroup>` (compile patterns + project references) + `</Project>` to each template fragment. The `.sln` is generated from a minimal template.
3. **Directory.Build.props** is written per variant with `$(ProjectRoot)` (absolute path) and `$(DefineConstants)` (platform + config defines). Both the props file and the templates use `$(DefineConstants)` with append semantics so static and dynamic defines are combined correctly.

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

## Performance

Benchmarked on a project with 19 assemblies (~26k source files across Assets/ and Packages/):

| Variant | Mean |
|---------|------|
| ios-prod | 26ms |
| ios-editor | 27ms |
| android-prod | 26ms |

No Foundation dependency — binary links only against libSystem, libswiftCore, libswiftDarwin, and libswiftDispatch. Filesystem scan runs in parallel via GCD (`concurrentPerform`), and template rendering is append-only with no string replacement passes.

## Unity project setup

After cloning, or after Unity upgrades / package changes, re-initialize templates:

```bash
unity-solution-generator init .
```
