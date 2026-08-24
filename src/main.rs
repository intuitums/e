//! The `e` binary: CLI subcommands and handoff to the interactive frame.
//!
//! Session UI lives in `tui::app` — this file owns flags, one-shot commands
//! (`auth`, `ask`, `docs`, `update`), then opens the frame loop.

use crossterm::terminal;

use e::core::agent::{Agent, SessionEvent};
use e::core::providers::catalog::{self as model};
use e::tui::app;

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

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-v") {
        println!("e {}", e::VERSION);
        return Ok(());
    }
    // Extensions start before normal argument parsing so the startup hook can
    // consume custom flags and safely relaunch this same binary in a new cwd,
    // and so --help can list the flags and commands extensions declare.
    let (jobs_tx, jobs_rx) = tokio::sync::mpsc::channel::<String>(256);
    let host = e::core::api::ExtensionHost::start(jobs_tx.clone()).await;
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
        host.shutdown().await;
        return Ok(());
    }
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

    match auth_status_requested(&args) {
        Ok(true) => {
            e::core::auth::login::auth_status();
            host.shutdown().await;
            return Ok(());
        }
        Ok(false) => {}
        Err(message) => {
            eprintln!("{message}");
            host.shutdown().await;
            std::process::exit(2);
        }
    }
    if args.first().map(String::as_str) == Some("ask") {
        return ask(args[1..].join(" "), host).await;
    }
    if args.first().map(String::as_str) == Some("update") {
        // Every one-shot exit owes extensions their shutdown notification.
        if e::core::update::is_dev_build() {
            println!("this is a dev build (under target/) — update with cargo, not e update");
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
        host.shutdown().await;
        std::process::exit(2);
    }
    let tty = e::tui::background::stdout_is_tty();
    let theme = e::tui::theme::resolve(&e::core::config::settings::theme(), false);
    let width = terminal::size()
        .map(|(c, _)| c as usize)
        .unwrap_or(80)
        .min(100);

    for warning in model::config_warnings() {
        eprintln!("warning: {warning}");
    }
    let (mut agent, mut events) = Agent::new(model::default_model());
    agent.set_host(host.clone());
    agent.submit(prompt, e::core::agent::context::system_prompt_here());

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
            SessionEvent::Warning(message) => {
                eprintln!("warning: {message}");
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

#[cfg(test)]
mod tests {
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn bare_auth_is_status_only() {
        assert_eq!(super::auth_status_requested(&args(&["auth"])), Ok(true));
        assert_eq!(
            super::auth_status_requested(&args(&["ask", "hello"])),
            Ok(false)
        );
    }

    #[test]
    fn auth_provider_argument_is_rejected_with_working_guidance() {
        let error = super::auth_status_requested(&args(&["auth", "openai-codex"])).unwrap_err();
        assert!(error.contains("usage: e auth"));
        assert!(error.contains("/login <provider>"));
    }
}
