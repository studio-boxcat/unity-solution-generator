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
    // `lock` only needs <root>; platform/config/extra-refs are irrelevant.
    // Default to "." so `resolve_project_root` climbs from CWD to the nearest
    // ancestor with `ProjectSettings/ProjectVersion.txt`.
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
    let inv = Invocation::parse_strict(args, "generate");
    match generate_sln(&inv) {
        Ok(sln_path) => {
            println!("{}", sln_path);
            ExitCode::SUCCESS
        }
        Err(code) => code,
    }
}

fn run_build(args: &[String]) -> ExitCode {
    // Split off `--`-passthrough first; everything before is the invocation,
    // everything after is forwarded verbatim to `dotnet build`.
    let (gen_args, dotnet_args): (&[String], &[String]) =
        match args.iter().position(|a| a == "--") {
            Some(i) => (&args[..i], &args[i + 1..]),
            None => (args, &[]),
        };

    let inv = Invocation::parse(gen_args);
    inv.print_banner("build");

    let sln_path = match generate_sln(&inv) {
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

/// Shared `generate`/`build` driver. Runs the generator, prints warnings to
/// stderr, returns the `.sln` path on success or an `ExitCode` on failure.
fn generate_sln(inv: &Invocation) -> Result<String, ExitCode> {
    let options = GenerateOptions::new(inv.project_root.clone(), inv.platform)
        .with_build_config(inv.config)
        .with_extra_refs(inv.extra_refs.clone());

    // Always route through `scan_and_write`: it serves the cached lockfile on
    // a fingerprint hit (~ms) and rescans on a miss. A bare `read` would use
    // a stale lockfile (e.g. dangling refs to deleted files → MSB3245).
    let lockfile = match LockfileIO::scan_and_write(&inv.project_root, DEFAULT_GENERATOR_ROOT) {
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
    let inv = Invocation::parse(args);
    inv.print_banner("typecheck");

    // Refresh `.csproj`/`.sln` so Rider/IDE consumers see the current
    // solution without a separate `generate` invocation. The fingerprint
    // cache short-circuits this in ~ms on no-op runs; full cost only on
    // actual changes. Same write path `build` uses internally.
    if let Err(code) = generate_sln(&inv) {
        return code;
    }

    let opts = TypecheckOptions::new(inv.project_root.clone(), inv.platform)
        .with_build_config(inv.config)
        .with_extra_refs(inv.extra_refs.clone());

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

/// Parsed CLI invocation shared across `generate`, `typecheck`, `build`.
///
/// Positionals: `[<root>] [<platform>] [<config>]` — all optional in `parse`,
/// `<platform>` and `<config>` required in `parse_strict` (used by
/// `generate`). Flag: `--extra-refs <comma-separated DLL paths>`.
///
/// `project_root` is post-`resolve_project_root` (absolute, with realpath
/// applied and the Unity-root climb performed). Subcommands use the same
/// `Invocation` for the banner and for the work itself — no double-parsing.
struct Invocation {
    project_root: String,
    platform: BuildPlatform,
    config: BuildConfig,
    extra_refs: Vec<DllRef>,
}

impl Invocation {
    fn parse(args: &[String]) -> Self {
        let (root, rest) = split_root_arg(args);
        let (platform, config, extra_refs) = parse_variant_args(rest);
        Self {
            project_root: resolve_project_root(&root),
            platform,
            config,
            extra_refs,
        }
    }

    /// Like `parse` but exits with usage when `<platform>` or `<config>` is
    /// absent. `generate` requires both — silently defaulting could ship the
    /// wrong variant to MSBuild.
    fn parse_strict(args: &[String], cmd: &str) -> Self {
        let (_, rest) = split_root_arg(args);
        if rest.len() < 2 {
            die(&format!(
                "{cmd} requires: [<unity-root>] <platform> <config> [options]"
            ));
        }
        Self::parse(args)
    }

    fn output_dir(&self) -> String {
        typecheck_output_dir(
            &self.project_root,
            DEFAULT_GENERATOR_ROOT,
            self.platform,
            self.config,
        )
    }

    /// One-line startup banner (variant + resolved root + output dir). On
    /// stderr so the `generate` stdout `.sln` path stays machine-parseable.
    fn print_banner(&self, cmd: &str) {
        eprintln!(
            "{cmd} {} {} at {} → {}",
            self.platform,
            self.config,
            self.project_root,
            self.output_dir(),
        );
    }
}

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

/// Split `<unity-root>` (optional) from the remaining positional args. The
/// first arg is treated as `<unity-root>` UNLESS it starts with `--` or
/// parses as a `BuildPlatform`, in which case `<unity-root>` defaults to "."
/// and the original args become the rest.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn defaults_to_ios_editor_when_no_positionals() {
        let inv = Invocation::parse(&[]);
        assert_eq!(inv.platform, BuildPlatform::Ios);
        assert_eq!(inv.config, BuildConfig::Editor);
        assert!(inv.extra_refs.is_empty());
    }

    #[test]
    fn parses_explicit_platform_and_config() {
        let inv = Invocation::parse(&s(&["/nope", "android", "dev"]));
        assert_eq!(inv.platform, BuildPlatform::Android);
        assert_eq!(inv.config, BuildConfig::Dev);
    }

    #[test]
    fn first_arg_is_platform_when_it_parses_as_one() {
        // Common case: `unity-solution-generator typecheck ios editor` — no
        // explicit root, "ios" is the platform.
        let inv = Invocation::parse(&s(&["ios", "prod"]));
        assert_eq!(inv.platform, BuildPlatform::Ios);
        assert_eq!(inv.config, BuildConfig::Prod);
    }

    #[test]
    fn parses_extra_refs_after_positionals() {
        let inv = Invocation::parse(&s(&[
            "/nope", "ios", "editor", "--extra-refs", "/a.dll,/b.dll",
        ]));
        assert_eq!(inv.extra_refs.len(), 2);
        assert!(inv.extra_refs[0].path.ends_with("/a.dll"));
        assert!(inv.extra_refs[1].path.ends_with("/b.dll"));
    }

    #[test]
    fn output_dir_lives_under_variant_obj_debug() {
        let inv = Invocation::parse(&s(&["/nope", "android", "dev"]));
        assert!(
            inv.output_dir().ends_with("/Library/UnitySolutionGenerator/android-dev/obj/Debug"),
            "unexpected output_dir: {}",
            inv.output_dir(),
        );
    }
}
