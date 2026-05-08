#!/bin/bash
set -euo pipefail

#---------------------------------------
# Configuration
#---------------------------------------

# --no-restore is `dotnet build`-only — added per-call; `dotnet msbuild` rejects it.
# Use `-v:q` (colon form) — `dotnet msbuild` doesn't accept the `-v q` (space) form
# that `dotnet build` allows.
BUILD_ARGS=(
  -v:q
  -nologo
  "-clp:ErrorsOnly;NoSummary"  # NoSummary suppresses dotnet's default summary; we print our own
  -p:WarningLevel=0
  "-p:NoWarn=MSB3277%3BCS2008%3BMSB3026"
  "-p:PathMap=$(pwd)=."        # shorten absolute paths in error messages
  -p:StopOnFirstFailure=false
  -p:UseSharedCompilation=true  # reuse persistent Roslyn compiler server across builds
  -p:GenerateDocumentationFile=false

  # RAR (ResolveAssemblyReference) optimizations — skip work unnecessary for compile checks.
  -p:_FindDependencies=false
  -p:ResolveAssemblyReferencesFindRelatedFiles=false
  -p:ResolveAssemblyReferencesFindSerializationAssemblies=false
  -p:ResolveAssemblyReferencesFindRelatedSatellites=false
  -p:ResolveAssemblyReferencesSilent=true
  -p:AutoUnifyAssemblyReferences=false
  -p:ResolveAssemblyWarnOrErrorOnTargetArchitectureMismatch=None
)

# Default args: skip IL emit (Roslyn writes a metadata-only ref assembly), pdb,
# analyzers, and post-compile target work irrelevant to "does it compile?".
# This is the default because the script's purpose is compile-validation —
# Unity does the real build. Pass `--emit` to opt into a full build (runnable
# IL assemblies + analyzers + pdb).
# Benchmarked ~2× faster than --emit on meow-tower (clean + touch+rebuild).
# Pitfall: alternating default and --emit invalidates the MSBuild up-to-date
# check (different output artifact at same path) → first run after toggle is
# a full rebuild. Pin each workflow to one mode.
NO_EMIT_ARGS=(
  -p:ProduceOnlyReferenceAssembly=true       # no IL, no method bodies — safe because we don't run the output
  -p:ProduceReferenceAssemblyInOutDir=true   # write straight to bin/, skip a copy
  -p:DebugType=none -p:DebugSymbols=false
  -p:RunAnalyzers=false
  -p:RunAnalyzersDuringBuild=false
  -p:_SkipAnalyzers=true                     # belt-and-suspenders — RunAnalyzers=false has been buggy historically
  -p:EnforceCodeStyleInBuild=false
  -p:RunCodeAnalysis=false
  -p:GenerateAssemblyInfo=false              # skip WriteCodeFragment
  -p:GenerateDependencyFile=false
  -p:GenerateRuntimeConfigurationFiles=false
  -p:CopyLocalLockFileAssemblies=false
  -p:GenerateSatelliteAssemblies=false
  -p:SatelliteResourceLanguages=en
  -p:_CheckForUnsupportedNETCoreVersion=false
  -p:_CheckForInvalidConfigurationAndPlatform=false
  -p:CopyDebugSymbolFilesFromPackages=false
  -p:CopyDocumentationFilesFromPackages=false
  -tl:off
)

#---------------------------------------
# Functions
#---------------------------------------

show_help() {
  cat << 'EOF'
Usage: build-unity-sln [platforms] [configs] [options]
       build-unity-sln --clean

Arguments:
  platforms      ios | android | osx | ios,android,osx (default: ios)
  configs        prod | dev | editor | dev,editor (default: editor)

  Comma-separated values build all combinations in parallel:
    build-unity-sln ios,android,osx editor   # 3 parallel builds

Default behavior is a fast compile-check: Roslyn emits metadata-only ref
assemblies (no IL, no method bodies), analyzers/pdb/post-compile copies are
skipped. ~2× faster than --emit. The output is NOT runnable — Unity does the
real build. See `--emit` if you need actual IL.

Options:
  --emit         Produce runnable IL assemblies (full build, analyzers on,
                 pdb generated). Slower; rarely needed since Unity rebuilds
                 the solution itself.
  --clean        Remove cached build artifacts. Mutually exclusive with --emit.
  --help, -h     Show this help message

Run from a Unity project root. Uses unity-solution-generator to produce a
variant solution with the correct defines. Build intermediates are cached
per variant in Library/UnitySolutionGenerator/{variant}/.
EOF
}

#---------------------------------------
# Parse arguments
#---------------------------------------

PLATFORMS=()
CONFIGS=()
CLEAN=false
EMIT=false

while [[ $# -gt 0 ]]; do
  case $1 in
    --clean)   CLEAN=true; shift ;;
    --emit)    EMIT=true; shift ;;
    --help|-h) show_help; exit 0 ;;
    *)
      IFS=',' read -ra tokens <<< "$1"
      all_platform=true all_config=true
      for t in "${tokens[@]}"; do
        case $t in ios|android|osx) ;; *) all_platform=false ;; esac
        case $t in prod|dev|editor) ;; *) all_config=false ;; esac
      done
      if $all_platform && [[ ${#PLATFORMS[@]} -eq 0 ]]; then
        PLATFORMS=("${tokens[@]}")
      elif $all_config && [[ ${#CONFIGS[@]} -eq 0 ]]; then
        CONFIGS=("${tokens[@]}")
      else
        echo "Unknown argument: $1"; show_help; exit 1
      fi
      shift
      ;;
  esac
done

#---------------------------------------
# Main
#---------------------------------------

command -v unity-solution-generator >/dev/null 2>&1 || { echo "error: unity-solution-generator not found in PATH"; exit 1; }
command -v dotnet >/dev/null 2>&1 || { echo "error: dotnet not found in PATH"; exit 1; }

if [[ ${#PLATFORMS[@]} -eq 0 ]]; then PLATFORMS=(ios); fi
if [[ ${#CONFIGS[@]} -eq 0 ]]; then CONFIGS=(editor); fi

if [[ "$CLEAN" == true && "$EMIT" == true ]]; then
  echo "error: --clean and --emit are mutually exclusive"; exit 1
fi

ACTION="build"
[[ "$CLEAN" == true ]] && ACTION="clean"

echo "build-unity-sln: platforms=${PLATFORMS[*]// /,} configs=${CONFIGS[*]// /,} emit=$EMIT clean=$CLEAN"

# Default (no-emit) mode persists the MSBuild process across invocations.
# Pairs with `dotnet msbuild` (vs `dotnet build`) to skip MSBuild assembly
# load + SDK resolve + targets graph parse on every run. See:
#   https://learn.microsoft.com/en-us/visualstudio/msbuild/msbuild-server
# Idle timeout configurable via MSBUILDNODECONNECTIONTIMEOUT (ms).
# Verify the server is running: pgrep -fa MSBuild
[[ "$EMIT" == false && "$CLEAN" == false ]] && export DOTNET_CLI_USE_MSBUILD_SERVER=1

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

build_variant() {
  local p=$1 c=$2 variant="${1}-${2}"
  (
    SLN=$(unity-solution-generator generate . "$p" "$c")
    if [[ "$CLEAN" == true ]]; then
      dotnet build "$SLN" -t:Clean --no-restore "${BUILD_ARGS[@]}"
    elif [[ "$EMIT" == true ]]; then
      dotnet build "$SLN" -m -graph --no-restore "${BUILD_ARGS[@]}"
    else
      # Default fast path. `dotnet msbuild` (not `dotnet build`) skips the
      # restore wrapper; `-noAutoResponse` skips MSBuild.rsp parsing; we drop
      # `-graph` because graph-mode evaluation overhead outweighs its
      # scheduling wins when most projects are no-op. `-m` keeps per-project
      # parallelism. Benchmarks: [[benchmark.md]].
      dotnet msbuild "$SLN" -m -noAutoResponse -nodeReuse:true \
        "${BUILD_ARGS[@]}" "${NO_EMIT_ARGS[@]}"
    fi
  ) > "$tmpdir/${variant}.log" 2>&1
}

pids=()
variants=()
for p in "${PLATFORMS[@]}"; do
  for c in "${CONFIGS[@]}"; do
    variant="${p}-${c}"
    for v in "${variants[@]+"${variants[@]}"}"; do [[ "$v" == "$variant" ]] && continue 2; done
    variants+=("$variant")
    build_variant "$p" "$c" &
    pids+=($!)
  done
done

failed=()
for i in "${!pids[@]}"; do
  if ! wait "${pids[$i]}"; then
    failed+=("${variants[$i]}")
  fi
done

# On failure: re-lock and retry only the failed variants
if [[ ${#failed[@]} -gt 0 ]]; then
  for v in "${failed[@]}"; do
    echo ""
    echo "=== ${v} errors (attempt 1) ==="
    cat "$tmpdir/${v}.log"
  done
  echo ""
  echo "${#failed[@]}/${#variants[@]} failed — re-locking and retrying..."
  unity-solution-generator lock . 2>&1

  retry_pids=()
  retry_variants=("${failed[@]}")
  for v in "${retry_variants[@]}"; do
    IFS='-' read -r p c <<< "$v"
    build_variant "$p" "$c" &
    retry_pids+=($!)
  done

  failed=()
  for i in "${!retry_pids[@]}"; do
    if ! wait "${retry_pids[$i]}"; then
      failed+=("${retry_variants[$i]}")
    fi
  done
fi

if [[ ${#failed[@]} -gt 0 ]]; then
  for v in "${failed[@]}"; do
    echo ""
    echo "=== ${v} errors (attempt 2) ==="
    cat "$tmpdir/${v}.log"
  done
  echo "FAILED: ${#failed[@]}/${#variants[@]} (${failed[*]})"
  exit 1
fi

echo "ok: ${#variants[@]}/${#variants[@]} (${variants[*]})"
