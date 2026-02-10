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
| `generate` (default) | Regenerate `.csproj`/`.sln` from manifest and filesystem |
| `init-manifest` | Generate `projects.json` from existing `.sln` and `asmdef` scan |
| `refresh-templates` | Extract templates from current Unity-generated `.csproj`/`.sln` |
| `prepare-build` | Generate platform-variant `.csproj` copies for device build validation |

```bash
# Generate solution files (default command)
unity-solution-generator --project-root /path/to/unity-project

# Bootstrap manifest from an existing Unity solution
unity-solution-generator init-manifest --project-root /path/to/unity-project

# Re-extract templates after Unity regenerates csproj/sln
unity-solution-generator refresh-templates --project-root /path/to/unity-project

# Generate iOS build-validation csprojs (outputs paths to stdout)
unity-solution-generator prepare-build --ios --manifest Library/UnitySolutionGenerator/projects.json --project-root /path/to/unity-project

# Generate Android build-validation csprojs, keeping DEBUG/TRACE defines
unity-solution-generator prepare-build --android --debug --manifest Library/UnitySolutionGenerator/projects.json --project-root /path/to/unity-project
```

All commands accept `--manifest <path>` to override the manifest file path relative to the project root. `prepare-build` requires `--manifest` explicitly.

## Build validation

`prepare-build` generates platform-variant `.csproj` copies that simulate device builds. It:

- Strips `UNITY_EDITOR` defines (no editor API on device)
- Strips `DEBUG`/`TRACE` defines unless `--debug` is passed
- Swaps platform defines (`UNITY_ANDROID` <-> `UNITY_IOS`)
- Removes `<ProjectReference>` entries for non-runtime projects (editor/test assemblies)
- Rewrites remaining project references to point at their platform-variant copies

Output is one `.csproj` path per line to stdout, for piping to `parallel` or `xargs`:

```bash
unity-solution-generator prepare-build --ios --manifest Library/UnitySolutionGenerator/projects.json --project-root . \
  | parallel dotnet build {} --no-restore -v q
```

Which projects are included is controlled by the `category` field in `projects.json` — only `"runtime"` projects get variants.

## How it works

1. **Manifest** (`projects.json`) declares each project's name, GUID, kind (`asmdef` or `legacy`), category (`runtime`, `editor`, or `test`), and template path.
2. **Templates** are Unity-generated `.csproj`/`.sln` files with placeholders (`{{SOURCE_FOLDERS}}`, `{{PROJECT_REFERENCES}}`, etc.) replacing the dynamic parts.
3. **Generate** scans `Assets/` and `Packages/` for `.cs` files, resolves ownership via `asmdef`/`asmref` assembly roots, builds recursive compile patterns with exclusions, and renders the templates.

### Source ownership resolution

For each `.cs` file, the generator walks the directory tree upward looking for the nearest `asmdef` or `asmref` assembly root. Files under an assembly root belong to that assembly. Files with no assembly root fall back to Unity's legacy assembly rules (`Assembly-CSharp`, `Assembly-CSharp-Editor`, etc.).

### Compile patterns

Instead of listing every `.cs` file individually, the generator emits recursive glob patterns:

```xml
<Compile Include="Assets/Game/**/*.cs" Exclude="Assets/Game/Tests/**/*.cs" />
```

Directories ending with `~` or starting with `.` are excluded from scanning and from glob patterns.

## Unity project setup

The Unity project needs:

```
Library/UnitySolutionGenerator/projects.json       # version controlled
Library/UnitySolutionGenerator/              # gitignored, regenerated
```

Typical workflow after cloning:

```bash
# 1. Extract templates from Unity-generated files
unity-solution-generator refresh-templates --project-root .

# 2. Regenerate solution (no Unity needed after this)
unity-solution-generator --project-root .
```

After Unity upgrades or package changes:

```bash
# Re-extract templates, then regenerate
unity-solution-generator refresh-templates --project-root .
unity-solution-generator --project-root .
```
