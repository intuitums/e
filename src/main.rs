//! The `e` binary: CLI subcommands and handoff to the interactive frame.
//!
//! Session UI lives in `tui::app` — this file owns flags, one-shot commands
//! (`auth`, `rpc`, `docs`, `update`, `help`), then opens the frame loop.
// Same contract as the library: no panic sites outside test builds.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable
    )
)]

use std::io::IsTerminal as _;

use e::core::agent::{Agent, SessionEvent};
use e::core::cli::{self, Options};
use e::core::providers::catalog::{self as model};
use e::tui::app;

/// Print the usage text shared by `e --help` and `e help`, including any
/// flags and commands that extensions contribute.
fn print_help(host: &e::core::extensions::ExtensionHost) {
    println!(
        "e — a coding agent for your terminal\n\n\
usage:\n  e [message]           start a session (optionally with a first prompt;\n                        piped stdin is not read — use -p or `e rpc` headless)\n  \
e -p, --print [msg]   run one turn headless and print the reply (the prompt\n                        is the argument, or piped stdin); with --json, stream\n                        every event as a JSON line, then a result line\n  \
e -c, --continue      continue this directory's most recent session\n  \
e -r, --resume        pick a session to resume\n  \
e rpc                 headless session server: JSONL over stdin/stdout\n                        (sessions, streaming events; `e docs automation`)\n  \
e docs [topic]        print a built-in format guide\n  \
e update              update e to the latest release\n  \
e install [source]    install a package, or make every listed one current\n  \
e remove <source>     forget a package and delete its clone\n  \
e packages            list installed packages\n  \
e packages init <dir> start a package to publish\n  \
e trust [dir]         trust a workspace's AGENTS.md, skills, and prompts\n  \
e untrust [dir]       stop loading them for that workspace\n  \
e auth                show sign-in status\n  \
e doctor [--no-network]\n                      print paste-safe, local-only runtime diagnostics\n  \
e providers           list provider support and sign-in state\n  \
e help                print this help\n  \
e -v, --version"
    );
    println!(
        "\nrun options:\n  \
--no-extensions, --ne  run without extensions\n  \
--no-save, --ns        keep the conversation in memory only\n  \
--no-tools, --nt       expose and run no tools\n  \
--model, -m <model>    select a model for this process\n  \
--effort, --ef <level> select reasoning effort for this process\n  \
--image, -i <path>     attach an image to the first prompt (repeatable)\n  \
--package, -P <source> load a package for this run only (repeatable)\n  \
--json, -j             machine output (doctor, providers, --print, --version)"
    );
    let flags = host.flags();
    let commands = host.commands();
    if !flags.is_empty() {
        println!("\nextension flags:");
        for (token, description) in flags {
            println!("  e {token:<20} {description}");
        }
    }
    if !commands.is_empty() {
        println!("\nextension commands:");
        for (name, description) in commands {
            println!("  /{name:<17} {description}");
        }
    }
    let shortcuts = host.shortcuts();
    if !shortcuts.is_empty() {
        println!("\nextension shortcuts:");
        for (chord, description) in shortcuts {
            println!("  {chord:<18} {description}");
        }
    }
}

fn auth_status_requested(args: &[String]) -> Result<bool, &'static str> {
    if args.first().map(String::as_str) != Some("auth") {
        return Ok(false);
    }
    if args.len() == 1 {
        Ok(true)
    } else {
        Err("usage: e auth\nSign in interactively with `/login <provider>`.")
    }
}

/// The subcommand when the first positional is one, unless a `--` delimiter
/// marked everything after it as prompt text. Parsing is strict, so the
/// positional head is the only place a subcommand can live.
fn leading_positional_subcommand(options: &Options) -> Option<&str> {
    if options.delimited {
        return None;
    }
    options.positional.first().map(String::as_str)
}

/// A single standalone word that is almost a subcommand is a typo, not a
/// prompt: suggest the real command instead of silently starting a session.
/// Multi-word input stays a prompt — only isolated words are judged.
fn unknown_command_hint(options: &Options) -> Option<String> {
    if options.delimited || options.positional.len() != 1 {
        return None;
    }
    let word = options.positional[0].as_str();
    if word == "version" {
        return Some("version is not a command — did you mean `e --version`?".into());
    }
    if cli::SUBCOMMANDS.contains(&word) {
        return None;
    }
    let suggestion = cli::did_you_mean(
        word,
        &cli::SUBCOMMANDS
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
    )?;
    Some(format!(
        "unknown command `{word}` — did you mean `e {suggestion}`?"
    ))
}

/// Report a usage error — themed on a terminal, JSON on stdout when
/// requested — shut extensions down, and exit with the usage status code.
async fn usage_error(host: &e::core::extensions::ExtensionHost, json: bool, message: String) -> ! {
    if json {
        println!("{}", serde_json::json!({"error": message}));
    } else if std::io::stderr().is_terminal() {
        let theme = e::tui::theme::resolve(&e::core::config::settings::theme(), false);
        eprintln!("{} {message}", theme.fg("error", "error:"));
    } else {
        eprintln!("error: {message}");
    }
    host.shutdown().await;
    std::process::exit(2);
}

/// `e install [source]`, `e remove <source>`, `e packages`: the package
/// commands, extension-free one-shots (a package's own broken extension must
/// never stand between the user and `e remove`). Returns the exit status.
async fn package_command(sub: &str, rest: &[String]) -> i32 {
    use e::core::resources::packages::{self, Status, KINDS};
    let plural = |n: usize, kind: &str| {
        let noun = kind.trim_end_matches('s');
        if n == 1 {
            format!("1 {noun}")
        } else {
            format!("{n} {kind}")
        }
    };
    let describe = |counts: &[usize; 4]| -> String {
        let parts: Vec<String> = KINDS
            .iter()
            .zip(counts)
            .filter(|(_, n)| **n > 0)
            .map(|(kind, n)| plural(*n, kind))
            .collect();
        if parts.is_empty() {
            "nothing to load".into()
        } else {
            parts.join(", ")
        }
    };
    match (sub, rest) {
        ("install", []) => {
            let results = packages::install_all().await;
            if results.is_empty() {
                println!(
                    "no packages listed — `e install <source>` adds one (see `e docs packages`)"
                );
                return 0;
            }
            let mut failed = false;
            for result in results {
                match result {
                    Ok(line) => println!("{line}"),
                    Err(line) => {
                        failed = true;
                        eprintln!("{line}");
                    }
                }
            }
            i32::from(failed)
        }
        ("install", [spec]) => match packages::install(spec).await {
            Ok((root, counts)) => {
                println!(
                    "installed {spec} → {} ({}) — restart or /reload to use it",
                    root.display(),
                    describe(&counts)
                );
                0
            }
            Err(message) => {
                eprintln!("{message}");
                1
            }
        },
        ("remove", [spec]) => match packages::remove(spec) {
            Ok(_) => {
                println!("removed {spec} — restart or /reload to drop it");
                0
            }
            Err(message) => {
                eprintln!("{message}");
                1
            }
        },
        ("packages", []) => {
            let list = packages::list();
            if list.is_empty() {
                println!(
                    "no packages listed — `e install <source>` adds one (see `e docs packages`)"
                );
                return 0;
            }
            let width = list.iter().map(|p| p.spec.len()).max().unwrap_or(0);
            for package in list {
                let status = match &package.status {
                    Status::Installed { counts, .. } => describe(counts),
                    Status::Missing => "missing — run `e install`".into(),
                    Status::Invalid(reason) => format!("invalid: {reason}"),
                };
                println!("{:<width$}  {status}", package.spec);
            }
            0
        }
        ("packages", [word, dir]) if word == "init" => {
            let dir = std::path::PathBuf::from(dir);
            match packages::init(&dir) {
                Ok(written) => {
                    println!("started a package in {}:", dir.display());
                    for path in written {
                        println!("  {}", path.strip_prefix(&dir).unwrap_or(&path).display());
                    }
                    println!("try it with `e --package {}`", dir.display());
                    0
                }
                Err(message) => {
                    eprintln!("{message}");
                    1
                }
            }
        }
        _ => {
            eprintln!("{}", cli::subcommand_usage(sub).unwrap_or("usage: e help"));
            2
        }
    }
}

/// `e trust [dir]` / `e untrust [dir]`: record a workspace's trust decision
/// without a terminal. An unattended session — a channel bot, a CI job — cannot
/// answer the trust panel, so without this the directory silently keeps loading
/// none of its own AGENTS.md, skills, prompts, or packages. Returns the exit
/// status.
fn trust_command(sub: &str, rest: &[String]) -> i32 {
    let trusted = sub == "trust";
    let requested = match rest {
        [] => match std::env::current_dir() {
            Ok(dir) => dir,
            Err(error) => {
                eprintln!("cannot read the current directory: {error}");
                return 1;
            }
        },
        [dir] => std::path::PathBuf::from(dir),
        _ => {
            eprintln!("usage: e {sub} [dir]");
            return 2;
        }
    };
    let dir = match requested.canonicalize() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("cannot use {}: {error}", requested.display());
            return 1;
        }
    };
    match e::core::config::trust::set(&dir, trusted) {
        Ok(()) if trusted => {
            println!(
                "trusted {} — its AGENTS.md, skills, and prompts load from now on",
                dir.display()
            );
            0
        }
        Ok(()) => {
            println!(
                "not trusted {} — its AGENTS.md, skills, and prompts stop loading",
                dir.display()
            );
            0
        }
        Err(error) => {
            eprintln!("cannot record the trust decision: {error}");
            1
        }
    }
}

/// Append the subcommand's usage line when the failing argv names one, so
/// `e doctor --unknown` points at `e doctor` instead of generic help.
fn with_subcommand_usage(message: String, args: &[String]) -> String {
    match cli::leading_subcommand(args).and_then(cli::subcommand_usage) {
        Some(usage) => format!("{message}\n{usage}"),
        None => message,
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if cli::has_flag(&args, &["--version", "-v"]) {
        if cli::has_flag(&args, &["--json"]) {
            println!(
                "{}",
                serde_json::json!({"version": e::VERSION, "channel": e::CHANNEL, "commit": e::COMMIT})
            );
        } else {
            println!("e {}", e::VERSION);
        }
        return Ok(());
    }
    // Diagnostics are deliberately extension-free: launching a user-owned
    // executable would violate `doctor`'s local/no-network contract before
    // the report could even begin. Extension flags cannot precede these
    // commands — stripping them requires starting extensions — so parsing
    // rejects such an argv instead of guessing. Other commands still start
    // extensions before normal argument parsing so the startup hook can
    // consume custom flags and safely relaunch this same binary in a new cwd,
    // and so --help can list the flags and commands extensions declare.
    let (jobs_tx, jobs_rx) = tokio::sync::mpsc::channel::<String>(256);
    let diagnostic_requested = matches!(
        cli::leading_subcommand(&args),
        Some("doctor" | "providers" | "install" | "remove" | "packages" | "trust" | "untrust")
    );
    // Extensions' own requests (`ui.*`, `session.*`) travel this channel
    // to whoever answers them: the terminal frontend, or `e rpc`, which
    // relays questions to its client. `e -p` starts the host without it,
    // so `initialize` tells extensions there is no UI.
    let headless = cli::has_flag(&args, &["--print", "-p"]);
    let (requests_tx, requests_rx) =
        tokio::sync::mpsc::channel::<e::core::extensions::HostRequest>(256);
    // `--package <source>` packages join this run before extensions start,
    // so their extensions launch like installed ones. A bad source is a
    // usage error, not a silent omission.
    if !diagnostic_requested {
        for spec in cli::flag_values(&args, &["--package", "-P"]) {
            if let Err(message) = e::core::resources::packages::use_once(&spec).await {
                eprintln!("--package {spec}: {message}");
                e::core::resources::packages::forget_once();
                std::process::exit(2);
            }
        }
    }
    let host = if cli::extensions_disabled(&args) || diagnostic_requested {
        e::core::extensions::ExtensionHost::empty()
    } else {
        e::core::extensions::ExtensionHost::start(
            jobs_tx.clone(),
            (!headless).then(|| requests_tx.clone()),
        )
        .await
    };
    if cli::has_flag(&args, &["--help", "-h"]) {
        print_help(&host);
        host.shutdown().await;
        return Ok(());
    }
    let startup_json_requested = cli::has_flag(&args, &["--json", "-j"]);
    // Diagnostics must remain available when a startup hook is the thing
    // being diagnosed. Extensions are initialized for health reporting, but
    // their startup hooks do not get to intercept or relaunch these commands.
    if let Ok(diagnostic_options) = cli::parse(args.clone(), &[]) {
        let diagnostic_args = &diagnostic_options.positional;
        let sub = leading_positional_subcommand(&diagnostic_options);
        // Package and trust commands are one-shots on the same extension-free
        // footing: they change what the next session loads, never the current
        // one.
        if let Some(sub @ ("install" | "remove" | "packages" | "trust" | "untrust")) = sub {
            if diagnostic_options.json {
                eprintln!("--json is supported by `e doctor` and `e providers`");
                std::process::exit(2);
            }
            let rest = &diagnostic_args[1..];
            let status = match sub {
                "trust" | "untrust" => trust_command(sub, rest),
                _ => package_command(sub, rest).await,
            };
            if status != 0 {
                std::process::exit(status);
            }
            return Ok(());
        }
        if sub == Some("doctor") || sub == Some("providers") {
            let doctor = sub == Some("doctor");
            // Parsing accepts `--no-network` (a no-op: diagnostics are
            // always local-only); any positional word after the command is
            // a usage error.
            // The block only runs when sub is one of those two words, so
            // the position always resolves; None simply skips the usage
            // check rather than panicking if that ever changes.
            if let Some(sub_idx) = diagnostic_args
                .iter()
                .position(|a| a == "doctor" || a == "providers")
            {
                if sub_idx != diagnostic_args.len() - 1 {
                    usage_error(
                        &host,
                        false,
                        if doctor {
                            "usage: e doctor [--no-network]".into()
                        } else {
                            "usage: e providers".into()
                        },
                    )
                    .await;
                }
            }

            let report = e::core::providers::diagnostics::report(&host);
            if diagnostic_options.json {
                let json = if doctor {
                    serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into())
                } else {
                    serde_json::to_string_pretty(&report.providers).unwrap_or_else(|_| "[]".into())
                };
                println!("{json}");
            } else if doctor {
                println!("{}", e::core::providers::diagnostics::render(&report));
            } else {
                for provider in &report.providers {
                    println!(
                        "{:<16} {:<10} {:<22} auth={:<8} models={}",
                        provider.name,
                        provider.tier,
                        provider.dialect,
                        if provider.signed_in {
                            provider.authentication.as_str()
                        } else {
                            "missing"
                        },
                        provider.models
                    );
                }
            }
            host.shutdown().await;
            return Ok(());
        }
    }
    match host.startup(args).await {
        Ok(e::core::extensions::StartupAction::Continue(next)) => args = next,
        Ok(e::core::extensions::StartupAction::Relaunch { argv, request }) => {
            host.shutdown().await;
            return app::relaunch_self(&request.cwd, &argv, &request.env);
        }
        Err(message) => {
            if startup_json_requested {
                println!("{}", serde_json::json!({"error": message}));
            } else {
                eprintln!("{message}");
            }
            host.shutdown().await;
            e::core::resources::packages::forget_once();
            std::process::exit(1);
        }
    }

    let json_requested = cli::has_flag(&args, &["--json", "-j"]);
    let extension_flags: Vec<String> = host.flags().into_iter().map(|(token, _)| token).collect();
    let options = match cli::parse(args.clone(), &extension_flags) {
        Ok(options) => options,
        Err(message) => {
            usage_error(&host, json_requested, with_subcommand_usage(message, &args)).await;
        }
    };
    let args = &options.positional;

    // Diagnostics were classified before extensions started (above); one
    // arriving here rode in behind an extension flag the raw scan could not
    // tell from a value-taking one. Refuse rather than send the word to the
    // model as a prompt with extensions running.
    if matches!(
        leading_positional_subcommand(&options),
        Some("doctor" | "providers")
    ) {
        usage_error(
            &host,
            json_requested,
            "diagnostics cannot follow extension flags — run `e doctor` or `e providers` first"
                .into(),
        )
        .await;
    }

    // One isolated near-miss word is a mistyped command, not a prompt.
    if let Some(message) = unknown_command_hint(&options) {
        usage_error(&host, false, message).await;
    }

    // `e help` is the subcommand form of `e --help`.
    if args.first().map(String::as_str) == Some("help") {
        if args.len() != 1 {
            usage_error(&host, false, "usage: e help".to_string()).await;
        }
        print_help(&host);
        host.shutdown().await;
        return Ok(());
    }
    match auth_status_requested(args) {
        Ok(true) => {
            if options.json {
                eprintln!("--json is supported by `e doctor` and `e providers`");
                host.shutdown().await;
                std::process::exit(2);
            }
            e::core::auth::login::auth_status();
            host.shutdown().await;
            return Ok(());
        }
        Ok(false) => {}
        Err(message) => {
            usage_error(&host, false, message.to_string()).await;
        }
    }
    if args.first().map(String::as_str) == Some("rpc") {
        return e::rpc::serve(host, &options, requests_rx, jobs_rx).await;
    }
    if options.print {
        // An unattended run has no dialog to answer, so an untrusted workspace
        // stops it here instead of quietly working without its instructions.
        if let Some(refusal) =
            e::core::config::trust::refusal(&std::env::current_dir().unwrap_or_default())
        {
            eprintln!("error: {refusal}");
            host.shutdown().await;
            std::process::exit(1);
        }
        let status = print_turn(host.clone(), &options, args).await;
        e::core::tools::kill_tracked_processes();
        host.shutdown().await;
        e::core::resources::packages::forget_once();
        if status != 0 {
            std::process::exit(status);
        }
        return Ok(());
    }
    if options.json {
        eprintln!("--json is supported by `e doctor`, `e providers`, and `-p`");
        host.shutdown().await;
        std::process::exit(2);
    }
    if args.first().map(String::as_str) == Some("update") {
        // Every one-shot exit owes extensions their shutdown notification.
        if e::core::update::is_dev_build() {
            println!("this is a dev build (under target/) — update with cargo, not e update");
            host.shutdown().await;
            return Ok(());
        }
        if !["stable", "dev", "beta"].contains(&e::CHANNEL)
            || !e::core::update::is_release_version(e::VERSION)
        {
            println!(
                "e {} is not a release build — update from source, not e update",
                e::VERSION
            );
            host.shutdown().await;
            return Ok(());
        }
        if e::core::update::target().is_none() {
            println!("{}", e::core::update::NO_RELEASE);
            host.shutdown().await;
            return Ok(());
        }
        match e::core::update::self_update().await {
            Ok(Some(version)) => println!("updated to e {version} — restart to use it"),
            Ok(None) => println!("e {} is already the latest", e::VERSION),
            Err(err) => {
                eprintln!("{err}");
                host.shutdown().await;
                std::process::exit(1);
            }
        }
        host.shutdown().await;
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("docs") {
        use e::core::resources::docs;
        match args.get(1).map(String::as_str) {
            Some(topic) => match docs::body(topic) {
                Some(text) => println!("{text}"),
                None => {
                    eprintln!("no such topic: {topic} — run `e docs` for the list");
                    host.shutdown().await;
                    std::process::exit(2);
                }
            },
            None => {
                println!("built-in guides — `e docs <topic>`:\n");
                for (name, blurb) in docs::topics() {
                    println!("  {name:<18} {blurb}");
                }
            }
        }
        host.shutdown().await;
        return Ok(());
    }

    // The interactive frame loop needs a terminal it owns. Piped stdin has
    // none, and headless one-shots go through `e rpc`, so refuse rather than
    // half-run a session with no way to read the keyboard.
    if !std::io::stdin().is_terminal() {
        usage_error(
            &host,
            false,
            "e needs an interactive terminal; for headless use `e rpc`".into(),
        )
        .await;
    }

    let selected = match cli::resolve_model(&options) {
        Ok(model) => model,
        Err(message) => {
            eprintln!("{message}");
            host.shutdown().await;
            std::process::exit(2);
        }
    };
    let initial = args.join(" ");
    if !options.images.is_empty() && initial.trim().is_empty() {
        eprintln!("--image requires an initial prompt");
        host.shutdown().await;
        std::process::exit(2);
    }
    // The trust panel answers a workspace nobody has decided about yet; a
    // recorded `false` is already an answer, and e only runs trusted.
    let cwd = std::env::current_dir().unwrap_or_default();
    if e::core::config::trust::status(&cwd) == Some(false) {
        if let Some(refusal) = e::core::config::trust::refusal(&cwd) {
            eprintln!("error: {refusal}");
        }
        host.shutdown().await;
        std::process::exit(1);
    }
    let images = match cli::load_images(&options, &selected) {
        Ok(images) => images,
        Err(message) => {
            eprintln!("{message}");
            host.shutdown().await;
            std::process::exit(2);
        }
    };
    let outcome = app::run(
        app::RunOptions {
            initial,
            continue_session: options.continue_session,
            resume_session: options.resume_session,
            model: selected,
            agent: cli::agent_options(&options),
            images,
        },
        host,
        jobs_tx,
        jobs_rx,
        (requests_tx, requests_rx),
    )
    .await;
    e::core::resources::packages::forget_once();
    outcome
}

/// `e -p [prompt]`: one headless turn. The prompt is the positional text,
/// or piped stdin when there is none. Plain mode streams the reply's text
/// to stdout as it arrives and puts warnings and errors on stderr; `--json`
/// streams every session event as one JSON line and ends with the same
/// result object `e rpc` returns. Exit status: 0 for a completed turn, 1
/// for an error or an interrupted turn, 2 for a usage problem.
async fn print_turn(
    host: std::sync::Arc<e::core::extensions::ExtensionHost>,
    options: &Options,
    args: &[String],
) -> i32 {
    use std::io::Write as _;
    let json = options.json;
    let fail = |message: String| -> i32 {
        if json {
            println!(
                "{}",
                serde_json::json!({"type": "result", "error": message})
            );
        } else {
            eprintln!("{message}");
        }
        2
    };
    let mut prompt = args.join(" ");
    if prompt.trim().is_empty() && !std::io::stdin().is_terminal() {
        let mut piped = String::new();
        if let Err(error) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut piped) {
            return fail(format!("could not read stdin: {error}"));
        }
        prompt = piped;
    }
    if prompt.trim().is_empty() {
        return fail("-p needs a prompt: an argument, or text on stdin".into());
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let selected = match cli::resolve_model(options) {
        Ok(selected) => selected,
        Err(error) => return fail(error),
    };
    let images = match cli::load_images(options, &selected) {
        Ok(images) => images,
        Err(error) => return fail(error),
    };
    let slug = model::slug(&selected);
    let pricing = selected.pricing.clone();
    let system = e::core::agent::context::system_prompt(&cwd);
    let (mut agent, mut events) = Agent::with_options(selected, cli::agent_options(options));
    let effort = agent.effort();
    agent.set_host(host);
    agent.submit_message(
        e::core::providers::ChatMessage::user_with_images(prompt, images),
        system,
    );

    let mut result = e::rpc::TurnAccumulator::with_warnings(model::config_warnings());
    let mut stdout = std::io::stdout();
    let mut printed_any = false;
    while let Some(event) = events.recv().await {
        result.observe(&event);
        if json {
            if let Some(line) = event.to_json() {
                println!("{line}");
            }
        } else {
            match &event {
                SessionEvent::TextDelta(delta) => {
                    let _ = stdout.write_all(delta.as_bytes());
                    let _ = stdout.flush();
                    printed_any = true;
                }
                SessionEvent::Warning(message) => eprintln!("warning: {message}"),
                SessionEvent::Retry {
                    attempt,
                    limit,
                    delay_secs,
                    reason,
                    ..
                } => eprintln!("retrying ({attempt}/{limit}) in {delay_secs}s: {reason}"),
                _ => {}
            }
        }
        if result.terminal {
            break;
        }
    }
    result.finish();
    let failed = result.failed();
    if json {
        let mut body = result.json(&slug, effort.as_deref(), pricing.as_ref());
        body["type"] = serde_json::Value::from("result");
        body["session"] = agent
            .session_path()
            .map(|p| serde_json::Value::from(p.display().to_string()))
            .unwrap_or(serde_json::Value::Null);
        println!("{body}");
    } else {
        if printed_any && !result.output.ends_with('\n') {
            println!();
        }
        if let Some(error) = &result.error {
            eprintln!("error: {error}");
        } else if result.aborted {
            eprintln!("turn interrupted");
        }
    }
    i32::from(failed)
}

#[cfg(test)]
mod tests {
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn bare_auth_is_status_only() {
        assert_eq!(super::auth_status_requested(&args(&["auth"])), Ok(true));
        assert_eq!(
            super::auth_status_requested(&args(&["doctor", "hello"])),
            Ok(false)
        );
    }

    #[test]
    fn auth_provider_argument_is_rejected_with_working_guidance() {
        let error = super::auth_status_requested(&args(&["auth", "openai-codex"])).unwrap_err();
        assert!(error.contains("usage: e auth"));
        assert!(error.contains("/login <provider>"));
    }

    #[test]
    fn near_miss_single_words_suggest_commands_not_sessions() {
        assert!(super::unknown_command_hint(&super::Options {
            positional: args(&["docss"]),
            ..Default::default()
        })
        .unwrap()
        .contains("did you mean `e docs`?"));
        assert_eq!(
            super::unknown_command_hint(&super::Options {
                positional: args(&["version"]),
                ..Default::default()
            })
            .unwrap(),
            "version is not a command — did you mean `e --version`?"
        );
        // Real subcommands, ordinary words, multi-word prompts, and
        // `--`-escaped text all stay prompts.
        for positional in [
            vec!["rpc".to_string()],
            vec!["help".to_string()],
            vec!["hello".to_string()],
            vec!["docss".to_string(), "world".to_string()],
        ] {
            assert_eq!(
                super::unknown_command_hint(&super::Options {
                    positional,
                    ..Default::default()
                }),
                None
            );
        }
        assert_eq!(
            super::unknown_command_hint(&super::Options {
                delimited: true,
                positional: args(&["docss"]),
                ..Default::default()
            }),
            None
        );
    }

    #[test]
    fn subcommand_head_is_none_when_delimited() {
        let options = super::Options {
            positional: args(&["doctor"]),
            delimited: true,
            ..Default::default()
        };
        assert_eq!(super::leading_positional_subcommand(&options), None);
        let options = super::Options {
            positional: args(&["doctor"]),
            ..Default::default()
        };
        assert_eq!(
            super::leading_positional_subcommand(&options),
            Some("doctor")
        );
    }
}
