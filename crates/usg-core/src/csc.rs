//! Direct `csc.dll` discovery + invocation, factored out of [`crate::typecheck`].
//! See [[architecture.md]] (Typecheck subsystem). This module knows how to find
//! the Roslyn compiler in an installed .NET SDK, render its `@response` file,
//! and drive it through `dotnet exec`; the typecheck orchestration (DAG walk,
//! UTD short-circuit) lives in `typecheck.rs`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lockfile::DllRef;

/// Filename of the resolved-csc.dll-path cache. Stored under `usg_cache_dir`
/// next to the tarball-extract cache — the SDK set is a per-host (not
/// per-project) invariant.
const CSC_DLL_CACHE_FILE: &str = "csc-dll-path";

pub(crate) fn find_csc_dll_cached(unity_version: &str) -> Option<String> {
    let cache_dir = crate::paths::usg_cache_dir(unity_version);
    let cache_path = crate::paths::join_path(&cache_dir, CSC_DLL_CACHE_FILE);
    // Fast path: read cached path, verify it still exists. SDK upgrades that
    // remove the previously-pinned csc.dll fall through to the slow path.
    if let Ok(cached) = fs::read_to_string(&cache_path) {
        let cached = cached.trim().to_string();
        if !cached.is_empty() && Path::new(&cached).exists() {
            return Some(cached);
        }
    }
    let resolved = find_csc_dll()?;
    // Best-effort cache write — a failure here just means the next invocation
    // pays the subprocess cost again. Surface it rather than swallow silently.
    if let Err(e) = fs::create_dir_all(&cache_dir) {
        tracing::warn!(target: "unity_solution_generator::typecheck", dir = %cache_dir, error = %e, "csc.dll cache dir create failed; will re-resolve next run");
    } else if let Err(e) = crate::io::write_file_if_changed(&cache_path, &resolved) {
        tracing::warn!(target: "unity_solution_generator::typecheck", path = %cache_path, error = %e, "csc.dll cache write failed; will re-resolve next run");
    }
    Some(resolved)
}

fn find_csc_dll() -> Option<String> {
    // Parse `dotnet --list-sdks` output: "8.0.303 [/usr/local/share/dotnet/sdk]"
    // → /usr/local/share/dotnet/sdk/8.0.303/Roslyn/bincore/csc.dll. Pick the
    // highest semver — `dotnet`'s own sort order isn't contractually
    // ascending, and a future 9.0/10.0 should win even if listed first.
    let out = Command::new("dotnet").arg("--list-sdks").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parse_semver = |s: &str| -> (u32, u32, u32) {
        let mut parts = s.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
        (
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
        )
    };
    let best = stdout
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() {
                return None;
            }
            let (version, rest) = l.split_once(' ')?;
            let base = rest.trim().trim_start_matches('[').trim_end_matches(']');
            Some((parse_semver(version), version.to_string(), base.to_string()))
        })
        .max_by_key(|t| t.0)?;
    let path = format!("{}/{}/Roslyn/bincore/csc.dll", best.2, best.1);
    if Path::new(&path).exists() {
        Some(path)
    } else {
        None
    }
}

/// Inputs to [`build_rsp`]. Named-field struct to keep the call site readable
/// and to avoid the clippy `too_many_arguments` lint as more flags get added.
pub(crate) struct BuildRspInputs<'a> {
    pub lang_version: &'a str,
    pub defines: &'a [String],
    pub refs: &'a [DllRef],
    pub proj_refs: &'a [PathBuf],
    pub analyzers: &'a [String],
    pub sources: &'a [PathBuf],
    pub out_dll: &'a str,
    pub allow_unsafe: bool,
}

pub(crate) fn build_rsp(i: &BuildRspInputs) -> String {
    // `/noconfig` MUST go on the command line (not in the rsp) — otherwise csc
    // emits CS2023 and reads its default csc.rsp anyway. See `invoke_csc`.
    let mut s = String::new();
    s.push_str("/nostdlib+\n");
    s.push_str("/target:library\n");
    // Intentionally NOT `/refonly` — under .NET 8 SDK csc (4.10.x) it silently
    // skips body-binding diagnostics that don't affect the reference-assembly
    // surface, e.g. CS1503 at call sites. Hit on meow-tower `orgel-fix`:
    // USG reported `ok` while Unity Editor flagged a real type mismatch. We
    // emit a full library; `/deterministic` keeps output byte-identical for
    // unchanged inputs so the mtime-restore cascade-skip trick still works.
    s.push_str("/deterministic\n");
    s.push_str(&format!("/langversion:{}\n", i.lang_version));
    s.push_str(&format!("/out:{}\n", i.out_dll));
    if i.allow_unsafe {
        s.push_str("/unsafe+\n");
    }
    if !i.defines.is_empty() {
        s.push_str(&format!("/define:{}\n", i.defines.join(";")));
    }
    for r in i.refs {
        s.push_str(&format!("/reference:{}\n", r.path));
    }
    for p in i.proj_refs {
        s.push_str(&format!("/reference:{}\n", p.display()));
    }
    for a in i.analyzers {
        s.push_str(&format!("/analyzer:{}\n", a));
    }
    for src in i.sources {
        s.push_str(&format!("{}\n", src.display()));
    }
    s
}

pub(crate) fn invoke_csc(csc_dll: &str, rsp_path: &str) -> std::result::Result<(), String> {
    let out = Command::new("dotnet")
        .arg("exec")
        .arg(csc_dll)
        // `/noconfig` and `/shared` are client-only flags and MUST go on the
        // command line (not in the rsp — csc otherwise rejects them with
        // CS2007 / CS2023). `/shared` connects to a long-lived VBCSCompiler
        // over the named-pipe protocol; the server amortizes Roslyn JIT +
        // metadata-loading across calls (~390 ms saved per call after the
        // first). VBCSCompiler self-spawns on first connect, idles 10 min.
        .arg("/shared")
        .arg("/noconfig")
        .arg(format!("@{}", rsp_path))
        .output()
        .map_err(|e| format!("failed to spawn dotnet: {}", e))?;
    if out.status.success() {
        Ok(())
    } else {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(filter_diagnostics(&format!("{}{}", stdout, stderr)))
    }
}

/// Strip `warning CS####` and `info CS####` / `info USG####` lines from csc
/// output. Typecheck is for errors only; warnings repeat across assemblies
/// that share sources via asmref (e.g. `com.boxcat.libs` pulled into half a
/// dozen projects), and they're not actionable from the typecheck path.
/// Errors and the csc banner pass through untouched.
fn filter_diagnostics(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for line in s.lines() {
        // csc diagnostics: `<path>(L,C): <severity> <CODE>: <text>` or
        // `<severity> <CODE>: <text>` for tool-level info. Drop everything
        // except errors. Includes `info SP####` from DiagnosticSuppressor,
        // `info USG####` (Unity), `warning CS####`, etc.
        if line.contains(": warning ") || line.contains(": info ") {
            continue;
        }
        let t = line.trim_start();
        if t.starts_with("warning ") || t.starts_with("info ") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Diagnostics-completeness contract: typecheck compiles *full* bodies, not
    /// reference-only assemblies, so csc surfaces body diagnostics Unity would
    /// catch. That includes CS1503 "cannot convert" at call sites — hit on the
    /// meow-tower `orgel-fix` branch where `/refonly` made USG report `ok`
    /// while Unity flagged `Argument 4: cannot convert from 'GameDate?' to
    /// 'OrgelDate?'`. `/refonly` must not appear; `/deterministic` must remain
    /// so the cascade-skip mtime-restore trick still works.
    #[test]
    fn rsp_has_no_refonly() {
        let sources = [PathBuf::from("/tmp/A.cs")];
        let rsp = build_rsp(&BuildRspInputs {
            lang_version: "9.0",
            defines: &[],
            refs: &[],
            proj_refs: &[],
            analyzers: &[],
            sources: &sources,
            out_dll: "/tmp/out.dll",
            allow_unsafe: false,
        });
        assert!(
            !rsp.lines().any(|l| l.trim() == "/refonly"),
            "rsp must not contain /refonly — it suppresses csc body diagnostics. rsp:\n{}",
            rsp
        );
        assert!(
            rsp.lines().any(|l| l.trim() == "/deterministic"),
            "rsp must keep /deterministic for cascade-skip stability. rsp:\n{}",
            rsp
        );
    }
}
