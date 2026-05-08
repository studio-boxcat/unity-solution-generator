use std::process::ExitCode;

use usg_core::{
    BuildConfig, BuildPlatform, DEFAULT_GENERATOR_ROOT, DllRef, GenerateOptions, LockfileIO,
    SolutionGenerator, TypecheckOptions, lockfile_path, resolve_project_root,
    typecheck::run as typecheck_run,
};

fn main() -> ExitCode {
    init_tracing();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return ExitCode::SUCCESS;
    }

    match args.first().map(String::as_str) {
        Some("lock") => {
            args.remove(0);
            run_lock(&args)
        }
        Some("generate") => {
            args.remove(0);
            run_generate(&args)
        }
        Some("typecheck") => {
            args.remove(0);
            run_typecheck(&args)
        }
        Some(other) => {
            die(&format!(
                "Unknown command '{}'. Use 'lock', 'generate', or 'typecheck'.",
                other
            ));
        }
        None => unreachable!(),
    }
}

fn run_lock(args: &[String]) -> ExitCode {
    // <unity-root> is optional — when omitted (or bare flags like --help land
    // here), default to "." so `resolve_project_root` climbs from CWD to the
    // nearest ancestor with `ProjectSettings/ProjectVersion.txt`.
    let unity_root = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str)
        .unwrap_or(".");
    let resolved = resolve_project_root(unity_root);
    match LockfileIO::scan_and_write(&resolved, DEFAULT_GENERATOR_ROOT) {
        Ok(lockfile) => {
            println!("Locked csproj.lock:");
            println!(
                "  Unity {} ({})",
                lockfile.unity_version, lockfile.unity_path
            );
            println!(
                "  {} DLL references, {} analyzers",
                lockfile.total_ref_count(),
                lockfile.analyzers.len()
            );
            println!(
                "  {} defines, {} scripting defines",
                lockfile.defines.len(),
                lockfile.defines_scripting.len()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(1)
        }
    }
}

fn run_generate(args: &[String]) -> ExitCode {
    let (project_root, rest) = split_root_arg(args);
    if rest.len() < 2 {
        die("generate requires: [<unity-root>] <platform> <config> [options]");
    }
    let Some(platform) = BuildPlatform::parse(&rest[0]) else {
        die(&format!(
            "Unknown platform '{}'. Use 'ios', 'android', or 'osx'.",
            rest[0]
        ));
    };
    let Some(build_config) = BuildConfig::parse(&rest[1]) else {
        die(&format!(
            "Unknown config '{}'. Use 'prod', 'dev', or 'editor'.",
            rest[1]
        ));
    };

    let mut extra_refs_raw: Option<String> = None;
    let mut i = 2;
    let args = rest;
    while i < args.len() {
        match args[i].as_str() {
            "--extra-refs" => {
                i += 1;
                if i >= args.len() {
                    die("--extra-refs requires a comma-separated list of DLL paths");
                }
                extra_refs_raw = Some(args[i].clone());
            }
            other => die(&format!("Unknown option: {}", other)),
        }
        i += 1;
    }

    let resolved = resolve_project_root(&project_root);
    let extra_refs = extra_refs_raw
        .as_deref()
        .map(DllRef::parse_list)
        .unwrap_or_default();
    let options = GenerateOptions::new(resolved.clone(), platform)
        .with_build_config(build_config)
        .with_extra_refs(extra_refs);

    // Lockfile is the only supported input now. If absent, scan-and-write it
    // before generating; the lock-fingerprint cache makes a redundant `lock`
    // call cheap.
    let lockfile_p = lockfile_path(&resolved, DEFAULT_GENERATOR_ROOT);
    let result = {
        let lockfile = if std::path::Path::new(&lockfile_p).exists() {
            match LockfileIO::read(&lockfile_p) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("error: {}", e);
                    return ExitCode::from(1);
                }
            }
        } else {
            eprintln!("No lockfile found, running lock...");
            match LockfileIO::scan_and_write(&resolved, DEFAULT_GENERATOR_ROOT) {
                Ok(l) => {
                    eprintln!("Locked: {}", l.unity_version);
                    l
                }
                Err(e) => {
                    eprintln!("error: {}", e);
                    return ExitCode::from(1);
                }
            }
        };
        SolutionGenerator::new().generate_from_lockfile(&options, &lockfile)
    };

    match result {
        Ok(r) => {
            println!("{}", r.variant_sln_path);
            for w in r.warnings {
                eprintln!("warning: {}", w);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(1)
        }
    }
}

fn run_typecheck(args: &[String]) -> ExitCode {
    // All positional args are optional. Defaults match the retired
    // build-unity-sln driver: platform=ios, config=editor. With no <unity-root>
    // (or none of the positionals), `resolve_project_root(".")` climbs from
    // CWD to the nearest ancestor with `ProjectSettings/ProjectVersion.txt`.
    let (project_root, rest) = split_root_arg(args);
    let platform = if !rest.is_empty() {
        match BuildPlatform::parse(&rest[0]) {
            Some(p) => p,
            None => die(&format!(
                "Unknown platform '{}'. Use 'ios', 'android', or 'osx'.",
                rest[0]
            )),
        }
    } else {
        BuildPlatform::Ios
    };
    let build_config = if rest.len() > 1 {
        match BuildConfig::parse(&rest[1]) {
            Some(c) => c,
            None => die(&format!(
                "Unknown config '{}'. Use 'prod', 'dev', or 'editor'.",
                rest[1]
            )),
        }
    } else {
        BuildConfig::Editor
    };

    let mut extra_refs_raw: Option<String> = None;
    let args = rest;
    let mut i = if args.len() > 1 { 2 } else { args.len() };
    while i < args.len() {
        match args[i].as_str() {
            "--extra-refs" => {
                i += 1;
                if i >= args.len() {
                    die("--extra-refs requires a comma-separated list of DLL paths");
                }
                extra_refs_raw = Some(args[i].clone());
            }
            other => die(&format!("Unknown option: {}", other)),
        }
        i += 1;
    }

    let resolved = resolve_project_root(&project_root);
    let extra_refs = extra_refs_raw
        .as_deref()
        .map(DllRef::parse_list)
        .unwrap_or_default();
    let opts = TypecheckOptions::new(resolved, platform)
        .with_build_config(build_config)
        .with_extra_refs(extra_refs);

    match typecheck_run(&opts) {
        Ok(result) => {
            if result.ok() {
                eprintln!(
                    "ok: {} recompiled, {} skipped",
                    result.recompiled, result.skipped
                );
                ExitCode::SUCCESS
            } else {
                for (name, msg) in &result.failures {
                    eprintln!("=== {} ===", name);
                    eprintln!("{}", msg);
                }
                eprintln!(
                    "FAILED: {}/{} project(s) ({})",
                    result.failures.len(),
                    result.failures.len() + result.recompiled + result.skipped,
                    result
                        .failures
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(1)
        }
    }
}

/// Split `<unity-root>` (optional) from the remaining positional args.
/// The first arg is treated as `<unity-root>` UNLESS it is empty, starts
/// with `--` (a flag), or parses as a `BuildPlatform` (`ios|android|osx`)
/// — in which case `<unity-root>` defaults to `"."` and the original args
/// become the rest.
fn split_root_arg(args: &[String]) -> (String, &[String]) {
    match args.first() {
        Some(first)
            if !first.starts_with("--") && BuildPlatform::parse(first).is_none() =>
        {
            (first.clone(), &args[1..])
        }
        _ => (".".to_string(), args),
    }
}

fn die(msg: &str) -> ! {
    eprintln!("error: {}", msg);
    std::process::exit(1);
}

/// `USG_PROFILE=1` enables a concise stderr profile (one line per `info` span,
/// with elapsed time). `USG_PROFILE=full` includes lower-level child spans too.
/// Default off — no overhead.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::fmt;

    let level = match std::env::var("USG_PROFILE").ok().as_deref() {
        None | Some("") | Some("0") => return,
        Some("full") => "trace",
        _ => "info",
    };
    let filter =
        EnvFilter::try_from_env("USG_LOG").unwrap_or_else(|_| EnvFilter::new(format!("usg_core={level}")));
    fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .with_span_events(fmt::format::FmtSpan::CLOSE)
        .with_env_filter(filter)
        .init();
}

fn print_usage() {
    println!(
        "USAGE:
  unity-solution-generator lock      [<unity-root>]
  unity-solution-generator generate  [<unity-root>] <platform> <config> [options]
  unity-solution-generator typecheck [<unity-root>] [<platform>] [<config>] [options]

  When <unity-root> is omitted, climbs from the current directory to the
  nearest ancestor containing `ProjectSettings/ProjectVersion.txt`.
  `typecheck` also defaults platform=ios, config=editor.

COMMANDS:
  lock                  Scan Unity installation and project to generate csproj.lock
  generate              Regenerate .csproj/.sln for a platform+config variant
  typecheck             Validate compile via direct csc.dll invocation
                        (no MSBuild). Used by Hot Reload pre-flight in
                        meow-tower's justfile.

ARGUMENTS:
  unity-root            Unity project root (defaults to climbing from CWD)
  platform              ios | android | osx
  config                prod | dev | editor

OPTIONS:
  --extra-refs <paths>  Comma-separated absolute paths to additional DLLs
  -h, --help            Show help"
    );
}
