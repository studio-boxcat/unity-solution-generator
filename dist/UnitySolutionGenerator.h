// UnitySolutionGenerator C API
// Link against libUnitySolutionGenerator.dylib

#ifndef UNITY_SOLUTION_GENERATOR_H
#define UNITY_SOLUTION_GENERATOR_H

#include <stdint.h>

// Generate .csproj/.sln from lockfile. Auto-runs the equivalent of
// `unity-solution-generator lock` if no lockfile exists.
// Returns 0 on success, nonzero on error (call usg_last_error).
//
// platform:    "ios", "android", or "osx"
// config:      "editor", "prod", or "dev"
// outputDir:   relative dir (e.g. "Library/hotreload/Solution"), "." for root, NULL for default
// extraRefs:   comma-separated absolute DLL paths, or NULL
//
// Single-threaded contract — caller must serialize calls (cache files
// aren't reentrant-safe). Unity calls naturally serialize via the main
// asset-import thread.
int32_t usg_generate(
    const char *projectRoot,
    const char *platform,
    const char *config,
    const char *outputDir,       // nullable
    const char *extraRefs        // nullable
);

// Last error message, or NULL. Valid until the next usg_ call.
const char *usg_last_error(void);

#endif
