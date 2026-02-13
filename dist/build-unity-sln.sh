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
Usage: build-unity-sln.sh [platform] [config] [options]
       build-unity-sln.sh --clean

Arguments:
  platform       ios | android (default: ios)
  config         prod | dev | editor (default: prod)

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

PLATFORM=""
CONFIG=""
CLEAN=false

while [[ $# -gt 0 ]]; do
  case $1 in
    ios|android)          PLATFORM=$1;  shift ;;
    prod|dev|editor)      CONFIG=$1;    shift ;;
    --clean)              CLEAN=true;   shift ;;
    --help|-h)            show_help; exit 0 ;;
    *)                    echo "Unknown argument: $1"; show_help; exit 1 ;;
  esac
done

#---------------------------------------
# Main
#---------------------------------------

if [[ -z "$PLATFORM" ]]; then echo "platform: ios (default)"; fi
if [[ -z "$CONFIG" ]]; then echo "config:   prod (default)"; fi

PLATFORM=${PLATFORM:-ios}
CONFIG=${CONFIG:-prod}

SLN=$(unity-solution-generator generate . "$PLATFORM" "$CONFIG")

if [[ "$CLEAN" == true ]]; then
  echo "Cleaning ${PLATFORM} (${CONFIG})..."
  dotnet build "$SLN" -t:Clean "${BUILD_ARGS[@]}"
  exit
fi

echo "Building for ${PLATFORM} (${CONFIG})..."
# NoSummary in -clp suppresses dotnet's default summary; print our own result
set +e
dotnet build "$SLN" -m -graph "${BUILD_ARGS[@]}"  # -graph: static dependency analysis for better parallelism
rc=$?
set -e
if [[ $rc -eq 0 ]]; then
  echo "Build succeeded."
else
  echo "Build failed. (exit code: $rc)"
  exit $rc
fi
