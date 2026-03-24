#!/bin/bash
set -euo pipefail

#---------------------------------------
# Configuration
#---------------------------------------

BUILD_ARGS=(
  --no-restore
  -v q
  -nologo
  "-clp:ErrorsOnly;NoSummary"  # NoSummary suppresses dotnet's default summary; we print our own
  -p:WarningLevel=0
  "-p:NoWarn=MSB3277%3BCS2008%3BMSB3026"
  "-p:PathMap=$(pwd)=."        # shorten absolute paths in error messages
  -p:StopOnFirstFailure=false
  -p:UseSharedCompilation=true  # reuse persistent Roslyn compiler server across builds
  -p:GenerateDocumentationFile=false

  # RAR (ResolveAssemblyReference) optimizations — skip work unnecessary for compile checks
  -p:_FindDependencies=false                                      # skip transitive dependency walking
  -p:ResolveAssemblyReferencesFindRelatedFiles=false              # skip .pdb/.xml probing
  -p:ResolveAssemblyReferencesFindSerializationAssemblies=false
  -p:ResolveAssemblyReferencesFindRelatedSatellites=false         # skip satellite resource discovery
  -p:ResolveAssemblyReferencesSilent=true                         # suppress RAR internal logging
  -p:AutoUnifyAssemblyReferences=false                            # skip version conflict resolution
  -p:ResolveAssemblyWarnOrErrorOnTargetArchitectureMismatch=None
)

#---------------------------------------
# Functions
#---------------------------------------

show_help() {
  cat << 'EOF'
Usage: build-unity-sln [platforms] [configs] [options]
       build-unity-sln --clean

Arguments:
  platforms      ios | android | ios,android (default: ios)
  configs        prod | dev | editor | dev,editor (default: prod)

  Comma-separated values build all combinations in parallel:
    build-unity-sln ios,android editor,dev   # 4 parallel builds

Options:
  --clean        Remove cached build artifacts
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

while [[ $# -gt 0 ]]; do
  case $1 in
    --clean)   CLEAN=true; shift ;;
    --help|-h) show_help; exit 0 ;;
    *)
      IFS=',' read -ra tokens <<< "$1"
      all_platform=true all_config=true
      for t in "${tokens[@]}"; do
        case $t in ios|android) ;; *) all_platform=false ;; esac
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

if [[ ${#PLATFORMS[@]} -eq 0 ]]; then echo "platform: ios (default)"; PLATFORMS=(ios); fi
if [[ ${#CONFIGS[@]} -eq 0 ]]; then echo "config:   prod (default)"; CONFIGS=(prod); fi

ACTION="Building"
[[ "$CLEAN" == true ]] && ACTION="Cleaning"

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

pids=()
variants=()

for p in "${PLATFORMS[@]}"; do
  for c in "${CONFIGS[@]}"; do
    variant="${p}-${c}"
    # skip duplicates (e.g. ios,ios)
    for v in "${variants[@]+"${variants[@]}"}"; do [[ "$v" == "$variant" ]] && continue 2; done
    variants+=("$variant")
    echo "${ACTION} ${variant}..."
    (
      SLN=$(unity-solution-generator generate . "$p" "$c")
      if [[ "$CLEAN" == true ]]; then
        dotnet build "$SLN" -t:Clean "${BUILD_ARGS[@]}"
      else
        dotnet build "$SLN" -m -graph "${BUILD_ARGS[@]}"
      fi
    ) > "$tmpdir/${variant}.log" 2>&1 &
    pids+=($!)
  done
done

# Wait for all builds and collect results
failed=()
for i in "${!pids[@]}"; do
  if ! wait "${pids[$i]}"; then
    failed+=("${variants[$i]}")
  fi
done

# Print errors from failed builds
if [[ ${#failed[@]} -gt 0 ]]; then
  for v in "${failed[@]}"; do
    echo ""
    echo "=== ${v} errors ==="
    cat "$tmpdir/${v}.log"
  done
  echo "${#failed[@]}/${#variants[@]} variant(s) failed: ${failed[*]}"
  exit 1
fi

echo "All ${#variants[@]} variant(s) succeeded."
