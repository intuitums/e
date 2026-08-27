//! Read-only provider and runtime diagnostics. No network calls and no
//! credential values: this is safe to paste into an issue.

use serde::Serialize;

use crate::core::auth::{self, Credential};

fn safe_base_url(raw: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(raw) else {
        return "<invalid URL redacted>".into();
    };
    // Custom gateways occasionally put credentials in userinfo or query
    // parameters. Neither is necessary to diagnose routing/dialect issues.
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
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub version: String,
    pub default_model: String,
    pub e_home: String,
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
    // User-declared providers are valid first-class deployments even though
    // they have no built-in registry entry. Report them explicitly as
    // experimental instead of making them disappear from diagnostics.
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

    Report {
        version: crate::VERSION.into(),
        default_model: super::catalog::slug(&super::catalog::default_model()),
        e_home: crate::core::config::home::home().display().to_string(),
        providers,
        extensions: host
            .identities()
            .into_iter()
            .map(|(name, version)| ExtensionDiagnostic { name, version })
            .collect(),
        warnings: super::catalog::config_warnings(),
    }
}

pub fn render(report: &Report) -> String {
    let mut lines = vec![
        format!("e {}", report.version),
        format!("home: {}", report.e_home),
        format!("default model: {}", report.default_model),
        format!("extensions: {} active", report.extensions.len()),
        String::new(),
        "providers:".into(),
    ];
    for provider in &report.providers {
        lines.push(format!(
            "  {:<16} {:<10} {:<22} auth={:<8} models={}",
            provider.name,
            provider.tier,
            provider.dialect,
            if provider.signed_in {
                provider.authentication.as_str()
            } else {
                "missing"
            },
            provider.models
        ));
        lines.push(format!(
            "    catalog={} responses_mount={} base={}",
            provider.catalog, provider.responses_mount, provider.base_url
        ));
    }
    if !report.extensions.is_empty() {
        lines.push(String::new());
        lines.push("extensions:".into());
        for extension in &report.extensions {
            lines.push(format!("  {} {}", extension.name, extension.version));
        }
    }
    if !report.warnings.is_empty() {
        lines.push(String::new());
        lines.push("warnings:".into());
        lines.extend(report.warnings.iter().map(|warning| format!("  {warning}")));
    }
    lines.join("\n")
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
