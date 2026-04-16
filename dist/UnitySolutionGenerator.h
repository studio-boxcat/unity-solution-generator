// UnitySolutionGenerator C API
// Link against libUnitySolutionGenerator.dylib

#ifndef UNITY_SOLUTION_GENERATOR_H
#define UNITY_SOLUTION_GENERATOR_H

#include <stdint.h>

// Generate .csproj/.sln from lockfile. Auto-runs lock if needed.
// Returns 0 on success, nonzero on error (call usg_last_error).
//
// platform:    "ios" or "android"
// config:      "editor", "prod", or "dev"
// outputDir:   relative dir (e.g. "Library/hotreload/Solution"), "." for root, NULL for default
// extraRefs:   comma-separated absolute DLL paths, or NULL
// slnPathOut:  buffer receives the output .sln path (relative to project root), or NULL to skip
int32_t usg_generate(
    const char *projectRoot,
    const char *platform,
    const char *config,
    const char *outputDir,       // nullable
    const char *extraRefs,       // nullable
    char *slnPathOut,            // nullable
    int32_t slnPathOutLen
);

// Scan Unity installation + project and write csproj.lock.
// Returns 0 on success, nonzero on error.
int32_t usg_lock(const char *projectRoot);

// Last error message, or NULL. Valid until the next usg_ call.
const char *usg_last_error(void);

#endif
