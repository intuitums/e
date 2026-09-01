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

use e::core::agent::{Agent, AgentOptions, SessionEvent};
use e::core::cli::{self, Options};
use e::core::providers::catalog::{self as model, Model};
use e::tui::app;

/// Print the usage text shared by `e --help` and `e help`, including any
/// flags and commands that extensions contribute.
fn print_help(host: &e::core::api::ExtensionHost) {
    println!(
        "e — a coding agent for your terminal\n\n\
usage:\n  e [message]           start a session (optionally with a first prompt;\n                        piped stdin counts as prompt text)\n  \
e -c, --continue      continue this directory's most recent session\n  \
e -r, --resume        pick a session to resume\n  \
e rpc                 JSONL request/response protocol on stdin/stdout\n  \
e docs [topic]        print a built-in format guide\n  \
e update              update e to the latest release\n  \
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
--json, -j             machine output (doctor, providers)"
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
async fn usage_error(host: &e::core::api::ExtensionHost, json: bool, message: String) -> ! {
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
        println!("e {}", e::VERSION);
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
    let diagnostic_requested =
        matches!(cli::leading_subcommand(&args), Some("doctor" | "providers"));
    let host = if cli::extensions_disabled(&args) || diagnostic_requested {
        e::core::api::ExtensionHost::empty()
    } else {
        e::core::api::ExtensionHost::start(jobs_tx.clone()).await
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
        Ok(e::core::api::StartupAction::Continue(next)) => args = next,
        Ok(e::core::api::StartupAction::Relaunch { argv, request }) => {
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
        return rpc(host, &options).await;
    }
    if options.json {
        eprintln!("--json is supported by `e doctor` and `e providers`");
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
        if !e::core::update::is_release_version(e::VERSION) {
            println!(
                "e {} is not a release build — update from source, not e update",
                e::VERSION
            );
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
                for (name, blurb) in docs::TOPICS {
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

    let selected = match resolve_model(&options) {
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
    let images = match load_images(&options, &selected) {
        Ok(images) => images,
        Err(message) => {
            eprintln!("{message}");
            host.shutdown().await;
            std::process::exit(2);
        }
    };
    app::run(
        app::RunOptions {
            initial,
            continue_session: options.continue_session,
            resume_session: options.resume_session,
            model: selected,
            agent: agent_options(&options),
            images,
        },
        host,
        jobs_tx,
        jobs_rx,
    )
    .await
}

fn resolve_model(options: &Options) -> Result<Model, String> {
    let selected = match options.model.as_deref() {
        Some(query) => model::resolve(query).ok_or_else(|| {
            format!(
                "model `{query}` is unavailable; sign in to its provider or choose a model from /model"
            )
        })?,
        None => model::default_model(),
    };
    if let Some(effort) = options.effort.as_deref() {
        if !selected.efforts.iter().any(|level| level == effort) {
            let supported = if selected.efforts.is_empty() {
                "none".to_string()
            } else {
                selected.efforts.join(", ")
            };
            return Err(format!(
                "model `{}` does not support effort `{effort}` (supported: {supported})",
                model::slug(&selected)
            ));
        }
    }
    Ok(selected)
}

fn agent_options(options: &Options) -> AgentOptions {
    AgentOptions {
        save_session: !options.no_save,
        tool_mode: options.tool_mode,
        effort_override: options.effort.clone(),
        allowed_tools: None,
    }
}

fn load_images(
    options: &Options,
    model: &Model,
) -> Result<Vec<e::core::providers::ImageInput>, String> {
    if !options.images.is_empty() && !model.image_input {
        return Err(format!(
            "model `{}` is not declared image-capable",
            model::slug(model)
        ));
    }
    e::core::providers::ImageInput::from_paths(&options.images)
}

#[derive(serde::Deserialize)]
struct RpcRequest {
    prompt: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    tool_mode: Option<String>,
    /// A positive tool allowlist for this turn: the turn sees only these
    /// built-ins. `None` is the full toolset; composes under `tool_mode`.
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    save: bool,
    #[serde(default)]
    images: Vec<String>,
}

fn rpc_options(defaults: &Options, request: &RpcRequest) -> Result<Options, String> {
    let requested_tools = match request.tool_mode.as_deref() {
        None | Some("all") => e::core::cli::ToolMode::All,
        Some("none") => e::core::cli::ToolMode::None,
        Some(other) => return Err(format!("unknown tool_mode `{other}`")),
    };
    let mut options = defaults.clone();
    options.model = request.model.clone().or(options.model);
    options.effort = request.effort.clone().or(options.effort);
    options.no_save = defaults.no_save || !request.save;
    options.images = request.images.clone();
    options.tool_mode = defaults.tool_mode.restrict(requested_tools);
    Ok(options)
}

#[derive(Default)]
struct TurnAccumulator {
    output: String,
    error: Option<String>,
    warnings: Vec<String>,
    aborted: bool,
    terminal: bool,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    tool_calls: u64,
    tool_failures: u64,
}

impl TurnAccumulator {
    fn with_warnings(warnings: Vec<String>) -> Self {
        Self {
            warnings,
            ..Self::default()
        }
    }

    fn observe(&mut self, event: &SessionEvent) {
        match event {
            SessionEvent::TextDelta(delta) => self.output.push_str(delta),
            SessionEvent::ToolBatchStart { calls } => self.tool_calls += calls.len() as u64,
            SessionEvent::ToolEnd { outcome, .. } if outcome.is_error() => {
                self.tool_failures += 1;
            }
            SessionEvent::Usage {
                input,
                output,
                cache_read,
            } => {
                self.input_tokens = self.input_tokens.saturating_add(*input);
                self.output_tokens = self.output_tokens.saturating_add(*output);
                self.cache_read_tokens = self.cache_read_tokens.saturating_add(*cache_read);
            }
            SessionEvent::Warning(warning) => self.warnings.push(warning.clone()),
            SessionEvent::Retry {
                attempt,
                limit,
                delay_secs,
                cause,
                reason,
            } => self.warnings.push(format!(
                "{} — retrying ({attempt}/{limit}) in {delay_secs}s: {reason}",
                cause.label()
            )),
            SessionEvent::Error(message) => self.error = Some(message.clone()),
            SessionEvent::TurnEnd { aborted } => {
                self.aborted = *aborted;
                self.terminal = true;
            }
            _ => {}
        }
    }

    fn finish(&mut self) {
        if !self.terminal && self.error.is_none() {
            self.error = Some("agent event stream closed before turn completion".into());
        }
    }

    fn json(
        &self,
        selected_model: &str,
        effort: Option<&str>,
        pricing: Option<&e::core::providers::catalog::Pricing>,
    ) -> serde_json::Value {
        let final_output = if self.error.is_none() && !self.aborted {
            self.output.as_str()
        } else {
            ""
        };
        serde_json::json!({
            "output": self.output,
            "final_output": final_output,
            "model": selected_model,
            "effort": effort,
            "aborted": self.aborted,
            "error": self.error,
            "warnings": self.warnings,
            "usage": {
                "input_tokens": self.input_tokens,
                "output_tokens": self.output_tokens,
                "cache_read_tokens": self.cache_read_tokens,
            },
            "cost_usd": pricing.map(|rates| rates.estimate(
                self.input_tokens,
                self.output_tokens,
                self.cache_read_tokens,
            )),
            "tools": {"calls": self.tool_calls, "failures": self.tool_failures},
        })
    }
}

/// Bound on one RPC request line — generous for pasted prompt text (images
/// travel as file paths, not inline bytes) but never unbounded: an
/// unterminated or malicious client must not grow this long-lived
/// process's memory without limit. Matches read_bounded_line's fail-fast
/// contract: hitting it ends the loop rather than skipping the line, since
/// a still-growing line with no newline yet cannot be safely resynced past.
const MAX_RPC_LINE_BYTES: usize = 10 * 1024 * 1024;

#[cfg(unix)]
struct RpcSignals {
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl RpcSignals {
    fn new() -> std::io::Result<Self> {
        use tokio::signal::unix::{signal, SignalKind};
        Ok(Self {
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
        })
    }

    async fn recv(&mut self) -> i32 {
        tokio::select! {
            _ = self.terminate.recv() => 143,
            _ = self.hangup.recv() => 129,
        }
    }
}

/// Stop owned shell groups before extension cleanup. `process::exit` is
/// intentional: Tokio's blocking stdin reader cannot be cancelled while RPC
/// is idle, so returning from main could leave signal shutdown hung forever.
#[cfg(unix)]
async fn exit_rpc_on_signal(
    host: &e::core::api::ExtensionHost,
    agent: Option<&mut Agent>,
    status: i32,
) -> ! {
    e::core::tools::kill_tracked_processes();
    if let Some(agent) = agent {
        agent.interrupt();
    }
    host.shutdown().await;
    std::process::exit(status);
}

/// A deliberately small machine protocol: sequential JSONL requests in,
/// exactly one JSON object out for each line. The extension host is reused,
/// while each request gets an isolated Agent and is memory-only by default.
async fn rpc(
    host: std::sync::Arc<e::core::api::ExtensionHost>,
    defaults: &Options,
) -> std::io::Result<()> {
    let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
    #[cfg(unix)]
    let mut signals = RpcSignals::new()?;
    loop {
        #[cfg(unix)]
        let line_result = tokio::select! {
            line = e::core::api::read_bounded_line(&mut reader, MAX_RPC_LINE_BYTES) => line,
            status = signals.recv() => exit_rpc_on_signal(&host, None, status).await,
        };
        #[cfg(not(unix))]
        let line_result = e::core::api::read_bounded_line(&mut reader, MAX_RPC_LINE_BYTES).await;
        let line = match line_result {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                // Fatal, same as a too-large extension line is fatal to its
                // reader: an oversized or unterminated line leaves the
                // stream mid-line with no safe resync point, so one error
                // response goes out and the process stops serving rather
                // than risk parsing the remainder of a giant line as if it
                // were fresh requests.
                println!(
                    "{}",
                    serde_json::json!({"id": null, "error": format!("invalid request: {error}")})
                );
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                println!(
                    "{}",
                    serde_json::json!({"id": null, "error": format!("invalid request: {error}")})
                );
                continue;
            }
        };
        let request_id = value.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let request: RpcRequest = match serde_json::from_value(value) {
            Ok(request) => request,
            Err(error) => {
                println!(
                    "{}",
                    serde_json::json!({"id": request_id, "error": format!("invalid request: {error}")})
                );
                continue;
            }
        };
        if let Some(unknown) = request
            .tools
            .as_ref()
            .and_then(|names| names.iter().find(|name| !e::core::tools::is_builtin(name)))
        {
            println!(
                "{}",
                serde_json::json!({
                    "id": request_id,
                    "error": format!("unknown built-in tool in allowlist: `{unknown}`")
                })
            );
            continue;
        }
        let options = match rpc_options(defaults, &request) {
            Ok(options) => options,
            Err(error) => {
                println!("{}", serde_json::json!({"id": request_id, "error": error}));
                continue;
            }
        };
        if request.prompt.trim().is_empty() {
            println!(
                "{}",
                serde_json::json!({"id": request_id, "error": "prompt is empty"})
            );
            continue;
        }
        let cwd = std::env::current_dir().unwrap_or_default();
        let selected = match resolve_model(&options) {
            Ok(selected) => selected,
            Err(error) => {
                println!("{}", serde_json::json!({"id": request_id, "error": error}));
                continue;
            }
        };
        let images = match load_images(&options, &selected) {
            Ok(images) => images,
            Err(error) => {
                println!("{}", serde_json::json!({"id": request_id, "error": error}));
                continue;
            }
        };
        let slug = model::slug(&selected);
        let pricing = selected.pricing.clone();
        let mut agent_opts = agent_options(&options);
        agent_opts.allowed_tools = request.tools.clone();
        // The turn uses e's ordinary system prompt. The generic tool policy
        // suffix records any request allowlist without adding a persona.
        let system = e::core::agent::context::system_prompt(&cwd);
        let (mut agent, mut events) = Agent::with_options(selected, agent_opts);
        let effort = agent.effort();
        agent.set_host(host.clone());
        agent.submit_message(
            e::core::providers::ChatMessage::user_with_images(request.prompt, images),
            system,
        );

        let mut result = TurnAccumulator::with_warnings(model::config_warnings());
        loop {
            #[cfg(unix)]
            let event = tokio::select! {
                event = events.recv() => event,
                status = signals.recv() => {
                    exit_rpc_on_signal(&host, Some(&mut agent), status).await
                }
            };
            #[cfg(not(unix))]
            let event = events.recv().await;
            let Some(event) = event else {
                break;
            };
            result.observe(&event);
            if result.terminal {
                break;
            }
        }
        result.finish();
        let mut body = result.json(&slug, effort.as_deref(), pricing.as_ref());
        body["id"] = request_id;
        // The saved session's JSONL path, when this turn persisted one: the
        // whole transcript — every tool call and its output — lives there, so
        // a caller that needs more than the final text can read it. Null when
        // the turn ran memory-only (`save` false).
        body["session"] = agent
            .session_path()
            .map(|p| serde_json::Value::from(p.display().to_string()))
            .unwrap_or(serde_json::Value::Null);
        println!("{body}");
    }
    e::core::tools::kill_tracked_processes();
    host.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use e::core::cli::{Options, ToolMode};
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
    fn rpc_cannot_relax_process_safety_flags() {
        let defaults = Options {
            no_save: true,
            tool_mode: ToolMode::None,
            ..Options::default()
        };
        let request = super::RpcRequest {
            prompt: "hello".into(),
            model: None,
            effort: None,
            tool_mode: Some("all".into()),
            tools: None,
            save: true,
            images: Vec::new(),
        };
        let resolved = super::rpc_options(&defaults, &request).unwrap();
        assert!(resolved.no_save);
        assert_eq!(resolved.tool_mode, ToolMode::None);
    }

    #[test]
    fn headless_stream_requires_a_terminal_event() {
        let mut result = super::TurnAccumulator::default();
        result.observe(&e::core::agent::SessionEvent::TextDelta("partial".into()));
        result.finish();
        assert!(result.error.is_some());
        assert_eq!(result.output, "partial");
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
