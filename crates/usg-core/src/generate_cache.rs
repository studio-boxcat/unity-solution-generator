//! Generate-result fingerprint cache.
//!
//! After a successful [`SolutionGenerator::generate_from_lockfile`], write a
//! tiny fingerprint file recording the inputs (scan-cache mtime, lockfile mtime,
//! canonical options string) and outputs (sln path, csproj paths). On the next
//! call with matching options, validate mtimes + check every output file still
//! exists; if so, reconstruct the [`GenerateResult`] and skip render+write
//! entirely.
//!
//! Storage: `Library/UnitySolutionGenerator/.fingerprints/<hash>` where `<hash>`
//! is a stable 64-bit SipHash13 of the canonical options string. Hash collisions
//! are caught by also writing the full options string into the file and
//! comparing on load (so a 64-bit collision would only trigger a needless
//! regenerate, never an incorrect cache hit).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

use crate::io::{create_dir_all, has_matching_version, read_file, write_file_if_changed};
use crate::paths::{DEFAULT_GENERATOR_ROOT, join_path, parent_directory};
use crate::solution_generator::{GenerateOptions, GenerateResult};

const FINGERPRINTS_DIR: &str = ".fingerprints";

/// Bump on incompatible generate-fingerprint format changes. Old fingerprints
/// will silently regenerate — never silently parse with stale meaning.
const GENERATE_FINGERPRINT_VERSION: u32 = 1;

/// Build the canonical options string. Anything that affects the rendered
/// output goes here; anything cosmetic (e.g. `verbose`, which only affects
/// warning printouts) does not.
fn canonical_options(opts: &GenerateOptions) -> String {
    let mut s = String::new();
    s.push_str("platform=");
    s.push_str(opts.platform.raw());
    s.push('|');
    s.push_str("config=");
    s.push_str(opts.build_config.raw());
    s.push('|');
    s.push_str("generator-root=");
    s.push_str(&opts.generator_root);
    s.push('|');
    s.push_str("output-dir=");
    match opts.output_dir.as_deref() {
        None => s.push_str("<default>"),
        Some(d) => s.push_str(d),
    }
    s.push('|');
    s.push_str("extra-refs=");
    for (i, r) in opts.extra_refs.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        // name|path so reordering doesn't collapse but path-only differences
        // (which would change the rendered <HintPath>) still register.
        s.push_str(&r.name);
        s.push('=');
        s.push_str(&r.path);
    }
    s
}

fn options_hash(canonical: &str) -> u64 {
    // DefaultHasher is SipHash13 with fixed keys (0, 0) — stable across runs.
    let mut h = DefaultHasher::new();
    canonical.hash(&mut h);
    h.finish()
}

pub(crate) fn fingerprint_path(project_root: &str, opts: &GenerateOptions) -> (String, String) {
    let canonical = canonical_options(opts);
    let hash = options_hash(&canonical);
    let dir = join_path(
        &join_path(project_root, &opts.generator_root),
        FINGERPRINTS_DIR,
    );
    let file = format!("{}/{:016x}", dir, hash);
    (canonical, file)
}

#[derive(Debug)]
pub(crate) struct Fingerprint {
    pub canonical_options: String,
    pub scan_cache_mtime: u128,
    pub lockfile_mtime: u128,
    pub sln: String,
    pub csprojs: Vec<String>,
    pub warnings: Vec<String>,
}

impl Fingerprint {
    fn serialize(&self) -> String {
        let mut s = String::from("# generate-fingerprint — auto-generated, do not edit\n");
        s.push_str(&format!("# version: {}\n", GENERATE_FINGERPRINT_VERSION));
        s.push_str("options: ");
        s.push_str(&self.canonical_options);
        s.push('\n');
        s.push_str(&format!("scan-cache-mtime: {}\n", self.scan_cache_mtime));
        s.push_str(&format!("lockfile-mtime: {}\n", self.lockfile_mtime));
        s.push_str("sln: ");
        s.push_str(&self.sln);
        s.push('\n');
        for c in &self.csprojs {
            s.push_str("csproj: ");
            s.push_str(c);
            s.push('\n');
        }
        for w in &self.warnings {
            s.push_str("warning: ");
            s.push_str(w);
            s.push('\n');
        }
        s
    }

    fn deserialize(content: &str) -> Option<Self> {
        let mut canonical_options = None;
        let mut scan_cache_mtime = None;
        let mut lockfile_mtime = None;
        let mut sln = None;
        let mut csprojs = Vec::new();
        let mut warnings = Vec::new();
        for line in content.split('\n') {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line.split_once(": ")?;
            match k {
                "options" => canonical_options = Some(v.to_string()),
                "scan-cache-mtime" => scan_cache_mtime = v.parse().ok(),
                "lockfile-mtime" => lockfile_mtime = v.parse().ok(),
                "sln" => sln = Some(v.to_string()),
                "csproj" => csprojs.push(v.to_string()),
                "warning" => warnings.push(v.to_string()),
                _ => {}
            }
        }
        Some(Fingerprint {
            canonical_options: canonical_options?,
            scan_cache_mtime: scan_cache_mtime?,
            lockfile_mtime: lockfile_mtime?,
            sln: sln?,
            csprojs,
            warnings,
        })
    }
}

/// Try to short-circuit a generate call. Returns `Some(result)` when every
/// validation passes:
///   1. Fingerprint file exists and parses.
///   2. Stored canonical options match the current call (catches 64-bit hash
///      collisions, which are statistically negligible but free to check).
///   3. `csproj.lock` exists and its mtime matches what the fingerprint recorded.
///   4. `scan-cache` exists and its mtime matches.
///   5. Every csproj/sln path that the fingerprint promises is still on disk.
///
/// Any failure → `None` and the caller proceeds with a normal generate.
pub(crate) fn try_load_valid(
    project_root: &str,
    opts: &GenerateOptions,
) -> Option<GenerateResult> {
    let (canonical, fp_path) = fingerprint_path(project_root, opts);
    let content = read_file(&fp_path).ok()?;
    if !has_matching_version(&content, GENERATE_FINGERPRINT_VERSION) {
        return None;
    }
    let fp = Fingerprint::deserialize(&content)?;
    if fp.canonical_options != canonical {
        return None;
    }

    let scan_cache = join_path(
        &join_path(project_root, &opts.generator_root),
        "scan-cache",
    );
    let lockfile = join_path(
        &join_path(project_root, DEFAULT_GENERATOR_ROOT),
        "csproj.lock",
    );
    let scan_mtime = mtime_nanos(&scan_cache)?;
    let lock_mtime = mtime_nanos(&lockfile)?;
    if scan_mtime != fp.scan_cache_mtime || lock_mtime != fp.lockfile_mtime {
        return None;
    }

    // Verify every output file is still on disk. If the user nuked the variant
    // dir (e.g. `just clean`), we must regenerate even if the input mtimes match.
    let sln_full = join_path(project_root, &fp.sln);
    if !Path::new(&sln_full).exists() {
        return None;
    }
    for c in &fp.csprojs {
        if !Path::new(&join_path(project_root, c)).exists() {
            return None;
        }
    }

    Some(GenerateResult {
        warnings: fp.warnings,
        variant_csprojs: fp.csprojs,
        variant_sln_path: fp.sln,
    })
}

pub(crate) fn write_after_generate(
    project_root: &str,
    opts: &GenerateOptions,
    result: &GenerateResult,
) {
    let (canonical, fp_path) = fingerprint_path(project_root, opts);
    let scan_cache = join_path(
        &join_path(project_root, &opts.generator_root),
        "scan-cache",
    );
    let lockfile = join_path(
        &join_path(project_root, DEFAULT_GENERATOR_ROOT),
        "csproj.lock",
    );
    let Some(scan_mtime) = mtime_nanos(&scan_cache) else {
        return;
    };
    let Some(lock_mtime) = mtime_nanos(&lockfile) else {
        return;
    };

    let fp = Fingerprint {
        canonical_options: canonical,
        scan_cache_mtime: scan_mtime,
        lockfile_mtime: lock_mtime,
        sln: result.variant_sln_path.clone(),
        csprojs: result.variant_csprojs.clone(),
        warnings: result.warnings.clone(),
    };
    create_dir_all(parent_directory(&fp_path));
    let _ = write_file_if_changed(&fp_path, &fp.serialize());
}

#[cfg(unix)]
fn mtime_nanos(path: &str) -> Option<u128> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    let secs = m.mtime();
    let nanos = m.mtime_nsec();
    if secs < 0 {
        return None;
    }
    Some((secs as u128) * 1_000_000_000 + (nanos as u128))
}

#[cfg(not(unix))]
fn mtime_nanos(path: &str) -> Option<u128> {
    let m = std::fs::metadata(path).ok()?;
    let mt = m.modified().ok()?;
    let d = mt.duration_since(std::time::SystemTime::UNIX_EPOCH).ok()?;
    Some(d.as_nanos())
}
