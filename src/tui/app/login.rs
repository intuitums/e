//! Sign-in flows: the /login menu, API-key capture, and the browser
//! OAuth / device-code launches whose outcomes land back in the frame loop.

use super::*;

impl App {
    /// Stop both the async flow and any blocking localhost callback wait.
    /// Returns whether a flow was active.
    pub(super) fn cancel_login(&mut self) -> bool {
        self.login_task.take().is_some()
    }

    pub(super) fn next_login_flow(&mut self) -> u64 {
        self.login_sequence = self.login_sequence.wrapping_add(1);
        self.login_sequence
    }

    pub(super) fn login_outcome_is_current(&self, flow_id: Option<u64>) -> bool {
        match flow_id {
            None => true,
            Some(flow_id) => self
                .login_task
                .as_ref()
                .is_some_and(|login| login.flow_id == flow_id),
        }
    }

    pub(super) fn start_codex_login(&mut self, provider: String) {
        self.cancel_login();
        let flow_id = self.next_login_flow();
        let cancellation = crate::core::auth::login::LoginCancellation::default();
        let task = tokio::spawn(crate::core::auth::login::codex_login(
            provider,
            self.jobs.clone(),
            self.logins.clone(),
            cancellation.clone(),
            flow_id,
        ));
        self.login_task = Some(ActiveLogin {
            flow_id,
            cancellation,
            task,
            wait_for_callback: true,
        });
    }

    pub(super) fn start_xai_login(&mut self) {
        self.cancel_login();
        let flow_id = self.next_login_flow();
        let cancellation = crate::core::auth::login::LoginCancellation::default();
        let task = tokio::spawn(crate::core::auth::login::xai_login(
            self.jobs.clone(),
            self.logins.clone(),
            cancellation.clone(),
            flow_id,
        ));
        self.login_task = Some(ActiveLogin {
            flow_id,
            cancellation,
            task,
            wait_for_callback: false,
        });
    }

    /// /login: the sign-in panel — the flow's method choice in the auth
    /// surface's own look.
    pub(super) fn open_login_menu(&mut self) {
        self.menu = None;
        self.cancel_login();
        self.auth = Some(AuthStage::Choose { selected: 0 });
    }

    /// A choice made on the panel. One account provider and one key provider
    /// today, so provider steps collapse straight through.
    pub(super) fn auth_choose(&mut self, selected: usize) {
        self.auth = Some(if selected == 0 {
            AuthStage::Account { selected: 0 }
        } else {
            AuthStage::Key { selected: 0 }
        });
    }

    /// A subscription picked on the account panel — the registry names the
    /// flow; this just dispatches it.
    pub(super) fn auth_account(&mut self, selected: usize) {
        let providers = crate::core::providers::registry::oauth_providers();
        let Some(provider) = providers.get(selected) else {
            return;
        };
        self.auth = Some(AuthStage::Waiting);
        self.notice(format!("starting the {} sign-in…", provider.display));
        match provider.auth.oauth.as_deref() {
            Some("xai-device") => self.start_xai_login(),
            _ => self.start_codex_login(provider.name.clone()),
        }
    }

    /// A provider picked on the API-key panel.
    pub(super) fn auth_key(&mut self, selected: usize) {
        let providers = crate::core::providers::registry::key_providers();
        let Some(provider) = providers.get(selected) else {
            return;
        };
        self.cancel_login();
        self.auth = Some(AuthStage::ApiKey {
            provider: provider.name.clone(),
        });
        self.pending_key = Some(provider.name.clone());
        self.editor.mask = true;
        self.editor.set_text("");
    }

    pub(super) fn submit_api_key(&mut self, key: &str) {
        let Some(secret_for) = self.pending_key.take() else {
            return;
        };
        self.auth = None;
        self.editor.mask = false;
        match crate::core::auth::login::save_api_key(&secret_for, key) {
            Ok(()) => {
                self.notice(format!("{secret_for}: key saved to ~/.e/auth.json"));
                // An API-key sign-in is a sign-in: emit the same typed
                // outcome the OAuth flows send, so the stranded-model
                // re-pick happens here too, not only for browser logins.
                let _ = self
                    .logins
                    .try_send(crate::core::auth::login::Outcome::SignedIn {
                        provider: secret_for,
                        flow_id: None,
                    });
            }
            Err(e) => self.notice(format!("{secret_for}: {e}")),
        }
    }

    /// `/login` — bare lists providers and methods; with a provider, runs
    /// that provider's method: Account (browser OAuth) or API key (masked
    /// paste into the composer).
    pub(super) fn login(&mut self, provider: String) {
        if provider.is_empty() {
            self.open_login_menu();
            return;
        }
        let flow =
            crate::core::providers::registry::find(&provider).and_then(|p| p.auth.oauth.clone());
        if flow.as_deref() == Some("codex") {
            self.notice(format!(
                "starting the {} sign-in…",
                model::display_name(&provider)
            ));
            self.auth = Some(AuthStage::Waiting);
            self.start_codex_login(provider);
        } else if flow.as_deref() == Some("xai-device") {
            self.notice(format!(
                "starting the {} sign-in…",
                model::display_name(&provider)
            ));
            self.auth = Some(AuthStage::Waiting);
            self.start_xai_login();
        } else {
            self.cancel_login();
            self.notice(format!(
                "paste the {provider} API key and press enter (esc cancels)"
            ));
            self.pending_key = Some(provider);
            self.editor.mask = true;
        }
    }
}
