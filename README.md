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
# Generate solution files (default command)
unity-solution-generator --project-root /path/to/unity-project

# Extract templates from Unity-generated csproj/sln
unity-solution-generator extract-templates --project-root /path/to/unity-project

# Generate iOS build-validation csprojs (outputs paths to stdout)
unity-solution-generator prepare-build --ios --project-root /path/to/unity-project

# Generate Android build-validation csprojs, keeping DEBUG/TRACE defines
unity-solution-generator prepare-build --android --debug --project-root /path/to/unity-project
```

All commands accept `--template-root <path>` (default: `Library/UnitySolutionGenerator`) to override the template directory relative to the project root.

## Build validation

`prepare-build` generates platform-variant `.csproj` copies that simulate device builds. It:

- Includes only projects whose `includePlatforms` matches the target (or has no platform restriction)
- Strips `UNITY_EDITOR` defines (no editor API on device)
- Strips `DEBUG`/`TRACE` defines unless `--debug` is passed
- Swaps platform defines (`UNITY_ANDROID` <-> `UNITY_IOS`)
- Removes `<ProjectReference>` entries for non-runtime and non-matching-platform projects
- Rewrites remaining project references to point at their platform-variant copies

Output is one `.csproj` path per line to stdout, for piping to `parallel` or `xargs`:

```bash
unity-solution-generator prepare-build --ios --project-root . \
  | parallel dotnet build {} --no-restore -v q
```

Which projects are runtime is determined automatically from `.asmdef` fields — see category inference below.

## How it works

1. **Templates** are Unity-generated `.csproj`/`.sln` files with placeholders (`{{SOURCE_FOLDERS}}`, `{{PROJECT_REFERENCES}}`, etc.) replacing the dynamic parts.
2. **Generate** discovers projects from the templates directory, scans `Assets/` and `Packages/` for `.cs` files, resolves ownership via `asmdef`/`asmref` assembly roots, builds per-directory compile patterns, and renders the templates.
3. **Project categories** (runtime/editor/test) are inferred from `.asmdef` fields — no manifest needed.

### Category inference

Categories are derived from `.asmdef` fields at runtime:

| Rule | Category |
|------|----------|
| `defineConstraints` contains `"UNITY_INCLUDE_TESTS"` | **test** |
| `includePlatforms` is exactly `["Editor"]` | **editor** |
| `defineConstraints` contains `"UNITY_EDITOR"` | **editor** |
| Everything else | **runtime** |

Platform-specific assemblies (e.g. `includePlatforms: ["iOS", "Editor"]`) are treated as **runtime**, but only included in `prepare-build` when the target platform matches. Projects without `.asmdef` files (legacy assemblies) are treated as **runtime** with no platform restriction.

### Source ownership resolution

For each `.cs` file, the generator walks the directory tree upward looking for the nearest `asmdef` or `asmref` assembly root. Files under an assembly root belong to that assembly. Files with no assembly root fall back to Unity's legacy assembly rules (`Assembly-CSharp`, `Assembly-CSharp-Editor`, etc.).

### Compile patterns

Instead of listing every `.cs` file individually, the generator emits per-directory glob patterns:

```xml
<Compile Include="Assets/Game/*.cs" />
<Compile Include="Assets/Game/Feature/*.cs" />
```

Directories ending with `~` or starting with `.` are excluded from scanning.

## Unity project setup

The Unity project needs:

```
Library/UnitySolutionGenerator/              # gitignored, regenerated templates
```

Typical workflow after cloning:

```bash
# 1. Extract templates from Unity-generated files
unity-solution-generator extract-templates --project-root .

# 2. Regenerate solution (no Unity needed after this)
unity-solution-generator --project-root .
```

After Unity upgrades or package changes:

```bash
# Extract templates, then regenerate
unity-solution-generator extract-templates --project-root .
unity-solution-generator --project-root .
```
