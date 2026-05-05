use std::process::ExitCode;

use usg_core::{
    BuildConfig, BuildPlatform, DEFAULT_GENERATOR_ROOT, DllRef, GenerateOptions, LockfileIO,
    SolutionGenerator, lockfile_path, resolve_project_root,
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
        Some("init") => {
            args.remove(0);
            eprintln!("warning: 'init' is deprecated, use 'lock' instead.");
            run_lock(&args)
        }
        Some("generate") => {
            args.remove(0);
            run_generate(&args)
        }
        Some(other) => {
            die(&format!(
                "Unknown command '{}'. Use 'lock', 'generate', or 'init'.",
                other
            ));
        }
        None => unreachable!(),
    }
}

fn run_lock(args: &[String]) -> ExitCode {
    let Some(unity_root) = args.first() else {
        die("lock requires: <unity-root>");
    };
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
    if args.len() < 3 {
        die("generate requires: <unity-root> <platform> <config> [options]");
    }
    let project_root = &args[0];
    let Some(platform) = BuildPlatform::parse(&args[1]) else {
        die(&format!(
            "Unknown platform '{}'. Use 'ios', 'android', or 'osx'.",
            args[1]
        ));
    };
    let Some(build_config) = BuildConfig::parse(&args[2]) else {
        die(&format!(
            "Unknown config '{}'. Use 'prod', 'dev', or 'editor'.",
            args[2]
        ));
    };

    let mut verbose = false;
    let mut output_dir: Option<String> = None;
    let mut extra_refs_raw: Option<String> = None;
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "-v" | "--verbose" => verbose = true,
            "--root" => output_dir = Some(".".to_string()),
            "-o" | "--output" => {
                i += 1;
                if i >= args.len() {
                    die("--output requires a directory argument");
                }
                output_dir = Some(args[i].clone());
            }
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

    let resolved = resolve_project_root(project_root);
    let extra_refs = extra_refs_raw
        .as_deref()
        .map(DllRef::parse_list)
        .unwrap_or_default();
    let options = GenerateOptions::new(resolved.clone(), platform)
        .with_build_config(build_config)
        .with_verbose(verbose)
        .with_output_dir(output_dir.as_deref())
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
  unity-solution-generator lock <unity-root>
  unity-solution-generator generate <unity-root> <platform> <config> [options]

COMMANDS:
  lock                  Scan Unity installation and project to generate csproj.lock
  generate              Regenerate .csproj/.sln for a platform+config variant
  init                  (deprecated) Alias for lock

ARGUMENTS:
  unity-root            Unity project root
  platform              ios | android | osx
  config                prod | dev | editor

OPTIONS:
  -o, --output <dir>    Output to <dir> (relative to project root) instead of variant dir
  --root                Alias for --output . (output to project root)
  --extra-refs <paths>  Comma-separated absolute paths to additional DLLs
  -v, --verbose         Print unresolved directory samples
  -h, --help            Show help"
    );
}
