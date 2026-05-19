use std::process::{Command, ExitCode};

use unity_solution_generator::{
    BuildConfig, BuildPlatform, DEFAULT_GENERATOR_ROOT, DllRef, GenerateOptions, LockfileIO,
    SolutionGenerator, TypecheckOptions, resolve_project_root,
    typecheck::{run as typecheck_run, typecheck_output_dir},
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
        Some("build") => {
            args.remove(0);
            run_build(&args)
        }
        Some(other) => {
            die(&format!(
                "Unknown command '{}'. Use 'lock', 'generate', 'typecheck', or 'build'.",
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
    match generate_sln(args, "generate", false) {
        Ok(sln_path) => {
            println!("{}", sln_path);
            ExitCode::SUCCESS
        }
        Err(code) => code,
    }
}

fn run_build(args: &[String]) -> ExitCode {
    // Split off `--`-passthrough first; everything before goes to generate,
    // everything after is forwarded verbatim to `dotnet build`.
    let (gen_args, dotnet_args): (&[String], &[String]) =
        match args.iter().position(|a| a == "--") {
            Some(i) => (&args[..i], &args[i + 1..]),
            None => (args, &[]),
        };

    let (project_root, rest) = split_root_arg(gen_args);
    let (platform, config, _) = parse_variant_args(rest);
    let resolved = resolve_project_root(&project_root);
    print_invocation_banner(
        "build",
        platform,
        config,
        &resolved,
        &typecheck_output_dir(&resolved, DEFAULT_GENERATOR_ROOT, platform, config),
    );

    let sln_path = match generate_sln(gen_args, "build", true) {
        Ok(p) => p,
        Err(code) => return code,
    };

    // Default to `-v:q` so MSBuild chatter doesn't bury real diagnostics.
    let default_args = ["-v:q".to_string()];
    let dotnet_args: &[String] = if dotnet_args.is_empty() { &default_args } else { dotnet_args };

    eprintln!("dotnet build {} {}", sln_path, dotnet_args.join(" "));
    match Command::new("dotnet").arg("build").arg(&sln_path).args(dotnet_args).status() {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(s) => ExitCode::from(s.code().unwrap_or(1).clamp(1, 255) as u8),
        Err(e) => {
            eprintln!("error: failed to spawn `dotnet`: {}", e);
            ExitCode::from(1)
        }
    }
}

/// Shared `generate`/`build` driver. Parses positional + `--extra-refs`,
/// runs the generator, prints any warnings to stderr, and returns the
/// `.sln` path on success or an `ExitCode` on failure. When
/// `defaults_allowed`, omitted platform/config fall back to `ios editor`
/// (matching `typecheck`); otherwise both are required.
fn generate_sln(
    args: &[String],
    cmd_name: &str,
    defaults_allowed: bool,
) -> Result<String, ExitCode> {
    let (project_root, rest) = split_root_arg(args);
    if !defaults_allowed && rest.len() < 2 {
        die(&format!(
            "{cmd_name} requires: [<unity-root>] <platform> <config> [options]"
        ));
    }
    let (platform, build_config, extra_refs) = parse_variant_args(rest);

    let resolved = resolve_project_root(&project_root);
    let options = GenerateOptions::new(resolved.clone(), platform)
        .with_build_config(build_config)
        .with_extra_refs(extra_refs);

    // Always route through `scan_and_write`: it serves the cached lockfile on
    // a fingerprint hit (~ms) and rescans on a miss. A bare `read` would use
    // a stale lockfile (e.g. dangling refs to deleted files → MSB3245).
    let lockfile = match LockfileIO::scan_and_write(&resolved, DEFAULT_GENERATOR_ROOT) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: {}", e);
            return Err(ExitCode::from(1));
        }
    };

    match SolutionGenerator::new().generate_from_lockfile(&options, &lockfile) {
        Ok(r) => {
            for w in r.warnings {
                eprintln!("warning: {}", w);
            }
            Ok(r.variant_sln_path)
        }
        Err(e) => {
            eprintln!("error: {}", e);
            Err(ExitCode::from(1))
        }
    }
}

fn run_typecheck(args: &[String]) -> ExitCode {
    // All positional args are optional. Defaults match the retired
    // build-unity-sln driver: platform=ios, config=editor. With no <unity-root>
    // (or none of the positionals), `resolve_project_root(".")` climbs from
    // CWD to the nearest ancestor with `ProjectSettings/ProjectVersion.txt`.
    let (project_root, rest) = split_root_arg(args);
    let (platform, build_config, extra_refs) = parse_variant_args(rest);

    let resolved = resolve_project_root(&project_root);
    print_invocation_banner(
        "typecheck",
        platform,
        build_config,
        &resolved,
        &typecheck_output_dir(&resolved, DEFAULT_GENERATOR_ROOT, platform, build_config),
    );

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

/// Parse `<platform> <config>` (both optional, defaulting to `ios editor`)
/// followed by `--extra-refs <paths>`. Shared by `typecheck` and the
/// `generate`/`build` driver. Callers that require both positionals enforce
/// that themselves before calling.
fn parse_variant_args(rest: &[String]) -> (BuildPlatform, BuildConfig, Vec<DllRef>) {
    let platform = match rest.first() {
        Some(s) => BuildPlatform::parse(s).unwrap_or_else(|| {
            die(&format!(
                "Unknown platform '{}'. Use 'ios', 'android', or 'osx'.",
                s
            ))
        }),
        None => BuildPlatform::Ios,
    };
    let build_config = match rest.get(1) {
        Some(s) => BuildConfig::parse(s).unwrap_or_else(|| {
            die(&format!(
                "Unknown config '{}'. Use 'prod', 'dev', or 'editor'.",
                s
            ))
        }),
        None => BuildConfig::Editor,
    };

    let mut extra_refs_raw: Option<String> = None;
    let mut i = rest.len().min(2);
    while i < rest.len() {
        match rest[i].as_str() {
            "--extra-refs" => {
                i += 1;
                if i >= rest.len() {
                    die("--extra-refs requires a comma-separated list of DLL paths");
                }
                extra_refs_raw = Some(rest[i].clone());
            }
            other => die(&format!("Unknown option: {}", other)),
        }
        i += 1;
    }
    let extra_refs = extra_refs_raw
        .as_deref()
        .map(DllRef::parse_list)
        .unwrap_or_default();
    (platform, build_config, extra_refs)
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

/// Shared one-line startup banner for `build` and `typecheck`. Variant +
/// resolved project root + the exact output dir so a user grepping logs can
/// see where artifacts landed without re-running with `USG_PROFILE`.
fn print_invocation_banner(
    cmd: &str,
    platform: BuildPlatform,
    config: BuildConfig,
    project_root: &str,
    output_dir: &str,
) {
    eprintln!(
        "{cmd} {} {} at {} → {}",
        platform.raw(),
        config.raw(),
        project_root,
        output_dir,
    );
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
        EnvFilter::try_from_env("USG_LOG").unwrap_or_else(|_| EnvFilter::new(format!("unity_solution_generator={level}")));
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
  unity-solution-generator build     [<unity-root>] [<platform>] [<config>] [options] [-- <dotnet-build-args>...]

  When <unity-root> is omitted, climbs from the current directory to the
  nearest ancestor containing `ProjectSettings/ProjectVersion.txt`.
  `typecheck` and `build` also default platform=ios, config=editor.

COMMANDS:
  lock                  Scan Unity installation and project to generate csproj.lock
  generate              Regenerate .csproj/.sln for a platform+config variant
  typecheck             Validate compile via direct csc.dll invocation
                        (no MSBuild). Used by Hot Reload pre-flight in
                        meow-tower's justfile.
  build                 Run `dotnet build` on the generated .sln. Args after
                        `--` are forwarded verbatim (defaults to `-v:q`).

ARGUMENTS:
  unity-root            Unity project root (defaults to climbing from CWD)
  platform              ios | android | osx
  config                prod | dev | editor

OPTIONS:
  --extra-refs <paths>  Comma-separated absolute paths to additional DLLs
  -h, --help            Show help"
    );
}
