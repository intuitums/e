//! The `e` binary: CLI subcommands and handoff to the interactive frame.
//!
//! Session UI lives in `tui::app` — this file owns flags, one-shot commands
//! (`auth`, `ask`, `docs`, `update`), then opens the frame loop.

use crossterm::terminal;

use e::core::agent::{Agent, SessionEvent};
use e::core::provider::catalog::{self as model};
use e::tui::app;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-v") {
        println!("e {}", e::VERSION);
        return Ok(());
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "e — a coding agent for your terminal\n\n\
usage:\n  e [message]           start a session (optionally with a first prompt)\n  \
e -c, --continue      continue this directory's most recent session\n  \
e -r, --resume        pick a session to resume\n  \
e ask \"prompt\"        one agent turn, no TUI; plain text when piped\n  \
e docs [topic]        print a built-in format guide\n  \
e update              update e to the latest release\n  \
e auth                show sign-in status\n  \
e -v, --version"
        );
        return Ok(());
    }
    // Extensions start before normal argument parsing so the startup hook can
    // consume custom flags and safely relaunch this same binary in a new cwd.
    let (jobs_tx, jobs_rx) = tokio::sync::mpsc::channel::<String>(256);
    let host = e::core::api::ExtensionHost::start(jobs_tx.clone()).await;
    match host.startup(args).await {
        Ok(e::core::api::StartupAction::Continue(next)) => args = next,
        Ok(e::core::api::StartupAction::Relaunch { argv, request }) => {
            host.shutdown().await;
            return app::relaunch_self(&request.cwd, &argv, &request.env);
        }
        Err(message) => {
            eprintln!("{message}");
            host.shutdown().await;
            std::process::exit(1);
        }
    }

    if args.first().map(String::as_str) == Some("auth") {
        e::core::auth::login::auth_status();
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("ask") {
        return ask(args[1..].join(" "), host).await;
    }
    if args.first().map(String::as_str) == Some("update") {
        if e::core::update::is_dev_build() {
            println!("this is a dev build (under target/) — update with cargo, not e update");
            return Ok(());
        }
        match e::core::update::self_update().await {
            Ok(Some(version)) => println!("updated to e {version} — restart to use it"),
            Ok(None) => println!("e {} is already the latest", e::VERSION),
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("docs") {
        use e::core::resources::docs;
        match args.get(1).map(String::as_str) {
            Some(topic) => match docs::body(topic) {
                Some(text) => println!("{text}"),
                None => {
                    eprintln!("no such topic: {topic} — run `e docs` for the list");
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
        return Ok(());
    }

    app::run(args, host, jobs_tx, jobs_rx).await
}

/// `e ask "prompt"` — one turn, no TUI. On a terminal the reply renders in
/// the full styled look once complete (tool activity streams as dim rows);
/// piped, raw text streams to stdout as it arrives. The session is saved
/// like any other, so `e -c` picks it up.
async fn ask(
    prompt: String,
    host: std::sync::Arc<e::core::api::ExtensionHost>,
) -> std::io::Result<()> {
    if prompt.trim().is_empty() {
        eprintln!("usage: e ask \"prompt\"");
        std::process::exit(2);
    }
    let tty = e::tui::background::stdout_is_tty();
    let theme = e::tui::theme::resolve(&e::core::config::settings::theme(), false);
    let width = terminal::size()
        .map(|(c, _)| c as usize)
        .unwrap_or(80)
        .min(100);

    let (mut agent, mut events) = Agent::new(model::default_model());
    agent.set_host(host.clone());
    agent.submit(prompt, app::system_prompt());

    use std::io::Write as _;
    let mut text = String::new();
    let mut failed = false;
    while let Some(event) = events.recv().await {
        match event {
            SessionEvent::TextDelta(d) => {
                if tty {
                    text.push_str(&d);
                } else {
                    print!("{d}");
                    let _ = std::io::stdout().flush();
                }
            }
            SessionEvent::ToolBatchStart { calls } => {
                if tty {
                    println!(
                        "{}",
                        theme.fg(
                            "dim",
                            &format!(
                                "● {} tool call{}",
                                calls.len(),
                                if calls.len() == 1 { "" } else { "s" }
                            )
                        )
                    );
                }
            }
            SessionEvent::ToolStart { .. } => {}
            SessionEvent::ToolOutput { chunk, .. } => {
                if tty {
                    for line in chunk.lines() {
                        println!("{}", theme.fg("dim", &format!("│ {line}")));
                    }
                }
            }
            SessionEvent::ToolEnd {
                summary, outcome, ..
            } => {
                if tty && outcome.is_error() {
                    println!("{}", theme.fg("dim", &format!("└ {summary}")));
                }
            }
            SessionEvent::Retry {
                attempt,
                limit,
                delay_secs,
                cause,
                reason,
            } => {
                eprintln!(
                    "{} — retrying ({attempt}/{limit}) in {delay_secs}s: {reason}",
                    cause.label()
                );
            }
            SessionEvent::Recovered { attempt, limit } => {
                eprintln!("recovered on attempt {attempt}/{limit}");
            }
            SessionEvent::Error(message) => {
                eprintln!("error: {message}");
                failed = true;
            }
            SessionEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    if tty && !text.is_empty() {
        println!();
        for line in e::tui::markdown::render_markdown(&theme, &text, width) {
            println!("{line}");
        }
    }
    if !tty {
        println!();
    }
    host.shutdown().await;
    if failed {
        std::process::exit(1);
    }
    Ok(())
}
