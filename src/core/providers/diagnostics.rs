//! Read-only provider and runtime diagnostics. No network calls and no
//! credential values: this is safe to paste into an issue.

use std::io::IsTerminal as _;
use std::path::Path;

use serde::Serialize;

use crate::core::auth::{self, Credential};

fn safe_base_url(raw: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(raw) else {
        return "<invalid URL redacted>".into();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string().trim_end_matches('/').to_string()
}

#[derive(Debug, Serialize)]
pub struct ProviderDiagnostic {
    pub name: String,
    pub display: String,
    pub tier: String,
    pub dialect: String,
    pub catalog: String,
    pub responses_mount: String,
    pub base_url: String,
    pub authentication: String,
    pub signed_in: bool,
    pub models: usize,
}

#[derive(Debug, Serialize)]
pub struct ExtensionDiagnostic {
    pub name: String,
    pub version: String,
    pub alive: bool,
}

#[derive(Debug, Serialize)]
pub struct TerminalDiagnostic {
    pub stdin_tty: bool,
    pub stdout_tty: bool,
    pub term: String,
}

#[derive(Debug, Serialize)]
pub struct ConfigurationDiagnostic {
    pub settings: String,
    pub auth: String,
    pub trust: String,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub version: String,
    pub target: String,
    pub working_directory: String,
    pub default_model: String,
    pub e_home: String,
    pub home_status: String,
    pub terminal: TerminalDiagnostic,
    pub configuration: ConfigurationDiagnostic,
    pub providers: Vec<ProviderDiagnostic>,
    pub extensions: Vec<ExtensionDiagnostic>,
    pub warnings: Vec<String>,
}

pub fn report(host: &crate::core::api::ExtensionHost) -> Report {
    let credentials = auth::load();
    let catalog = super::catalog::catalog();
    let mut providers = Vec::new();
    for provider in super::registry::all() {
        let authentication = match credentials.get(&provider.name) {
            Some(Credential::ApiKey { .. }) => "api_key",
            Some(Credential::OAuth { .. }) => "oauth",
            None if provider.auth.none => "none",
            None => "missing",
        };
        providers.push(ProviderDiagnostic {
            name: provider.name.clone(),
            display: provider.display.clone(),
            tier: provider.tier.as_str().into(),
            dialect: provider.api().as_str().into(),
            catalog: provider.catalog.as_str().into(),
            responses_mount: provider.responses_mount.as_str().into(),
            base_url: safe_base_url(&provider.base_url),
            authentication: authentication.into(),
            signed_in: auth::signed_in(&credentials, &provider.name),
            models: catalog
                .iter()
                .filter(|model| model.provider == provider.name)
                .count(),
        });
    }
    for model in &catalog {
        if super::registry::find(&model.provider).is_some()
            || providers
                .iter()
                .any(|provider| provider.name == model.provider)
        {
            continue;
        }
        let authentication = match credentials.get(&model.provider) {
            Some(Credential::ApiKey { .. }) => "api_key",
            Some(Credential::OAuth { .. }) => "oauth",
            None => "missing",
        };
        providers.push(ProviderDiagnostic {
            name: model.provider.clone(),
            display: model.provider.clone(),
            tier: "experimental".into(),
            dialect: model.api.as_str().into(),
            catalog: model.catalog.as_str().into(),
            responses_mount: model.responses_mount.as_str().into(),
            base_url: safe_base_url(&model.base_url),
            authentication: authentication.into(),
            signed_in: credentials.contains_key(&model.provider),
            models: catalog
                .iter()
                .filter(|candidate| candidate.provider == model.provider)
                .count(),
        });
    }

    let home = crate::core::config::home::home();
    Report {
        version: crate::VERSION.into(),
        target: crate::core::update::target().into(),
        working_directory: std::env::current_dir()
            .map(|path| sanitize_line(&path.display().to_string()))
            .unwrap_or_else(|_| "<unavailable>".into()),
        default_model: super::catalog::slug(&super::catalog::default_model()),
        e_home: sanitize_line(&home.display().to_string()),
        home_status: home_status(&home),
        terminal: TerminalDiagnostic {
            stdin_tty: std::io::stdin().is_terminal(),
            stdout_tty: std::io::stdout().is_terminal(),
            term: safe_env("TERM"),
        },
        configuration: ConfigurationDiagnostic {
            settings: json_status(
                &crate::core::config::home::settings_path(),
                crate::core::config::settings::FORMAT_VERSION,
            ),
            auth: json_status(
                &crate::core::config::home::auth_path(),
                crate::core::auth::FORMAT_VERSION,
            ),
            trust: json_status(
                &home.join("trust.json"),
                crate::core::config::trust::FORMAT_VERSION,
            ),
        },
        providers,
        extensions: host
            .diagnostic_status()
            .into_iter()
            .map(|(name, version, alive)| ExtensionDiagnostic {
                name: sanitize_line(&name),
                version: sanitize_line(&version),
                alive,
            })
            .collect(),
        warnings: super::catalog::config_warnings()
            .into_iter()
            .map(|warning| sanitize_line(&warning))
            .collect(),
    }
}

pub fn render(report: &Report) -> String {
    let mut lines = vec![
        "e doctor".into(),
        format!("version: e {}", sanitize_line(&report.version)),
        format!("target: {}", sanitize_line(&report.target)),
        format!("working directory: {}", report.working_directory),
        format!("home: {}", report.e_home),
        format!("home status: {}", sanitize_line(&report.home_status)),
        format!(
            "terminal: stdin={} stdout={} TERM={}",
            yes_no(report.terminal.stdin_tty),
            yes_no(report.terminal.stdout_tty),
            sanitize_line(&report.terminal.term)
        ),
        "configuration:".into(),
        format!(
            "  settings.json: {}",
            sanitize_line(&report.configuration.settings)
        ),
        format!("  auth.json: {}", sanitize_line(&report.configuration.auth)),
        format!(
            "  trust.json: {}",
            sanitize_line(&report.configuration.trust)
        ),
        format!("default model: {}", sanitize_line(&report.default_model)),
        format!("extensions: {} active", report.extensions.len()),
        String::new(),
        "providers:".into(),
    ];
    for provider in &report.providers {
        lines.push(format!(
            "  {:<16} {:<10} {:<22} auth={:<8} models={}",
            sanitize_line(&provider.name),
            sanitize_line(&provider.tier),
            sanitize_line(&provider.dialect),
            if provider.signed_in {
                provider.authentication.as_str()
            } else {
                "missing"
            },
            provider.models
        ));
        lines.push(format!(
            "    catalog={} responses_mount={} base={}",
            sanitize_line(&provider.catalog),
            sanitize_line(&provider.responses_mount),
            sanitize_line(&provider.base_url)
        ));
    }
    if !report.extensions.is_empty() {
        lines.push(String::new());
        lines.push("extensions:".into());
        for extension in &report.extensions {
            let version = if extension.version.is_empty() {
                "version unspecified".to_string()
            } else {
                format!("version {}", sanitize_line(&extension.version))
            };
            lines.push(format!(
                "  {}: {version}, {}",
                sanitize_line(&extension.name),
                if extension.alive { "running" } else { "exited" }
            ));
        }
    }
    if !report.warnings.is_empty() {
        lines.push(String::new());
        lines.push("warnings:".into());
        lines.extend(
            report
                .warnings
                .iter()
                .map(|warning| format!("  {}", sanitize_line(warning))),
        );
    }
    lines.join("\n")
}

fn home_status(path: &Path) -> String {
    if !path.exists() {
        return "missing (created on first write)".into();
    }
    if !path.is_dir() {
        return "error: path is not a directory".into();
    }
    let probe = path.join(format!(".doctor-write-{}", uuid::Uuid::new_v4()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(file) => {
            drop(file);
            match std::fs::remove_file(&probe) {
                Ok(()) => "writable".into(),
                Err(error) => format!("writable; probe cleanup failed: {error}"),
            }
        }
        Err(error) => format!("not writable: {error}"),
    }
}

fn json_status(path: &Path, supported: u32) -> String {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return "missing".into(),
        Err(error) => return format!("unreadable: {error}"),
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(serde_json::Value::Object(object)) => serde_json::Value::Object(object),
        Ok(_) => return "invalid: root is not an object".into(),
        Err(error) => return format!("invalid JSON: {error}"),
    };
    match value.get("format_version") {
        None => "valid, legacy unversioned format".into(),
        Some(version) if version.as_u64() == Some(u64::from(supported)) => {
            format!("valid, format {supported}")
        }
        Some(version) => format!("valid JSON, unsupported format {version}"),
    }
}

fn safe_env(name: &str) -> String {
    std::env::var(name)
        .ok()
        .map(|value| truncate(&sanitize_line(&value), 80))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unset".into())
}

fn sanitize_line(value: &str) -> String {
    crate::core::tools::sanitize_display(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate(value: &str, max: usize) -> String {
    let mut chars = value.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn provider_urls_do_not_expose_userinfo_queries_or_fragments() {
        let raw = format!(
            "https://{}@localhost/v1?api_key=also-secret#private",
            "alice:secret"
        );
        let safe = super::safe_base_url(&raw);
        assert_eq!(safe, "https://localhost/v1");
    }
}
