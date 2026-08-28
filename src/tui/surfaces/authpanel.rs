//! The sign-in panel — the auth surface's own look, distinct from the
//! generic picker: three-space indented prose rows, choices marked with a
//! `› ` caret (the one place a caret appears), descriptions at an absolute
//! column, and per-stage rows. Everything dim except the selected choice or
//! the live input row.
//!
//! Stages: Choose (account vs API key, the reference flow's wording) →
//! Account or Key (which provider, labeled by display name) → ApiKey (inline
//! `┃ •••` entry mirroring the composer) or Waiting (browser authorization
//! in flight) → Done (the outcome beat, returning to the list).
//!
//! Navigation contract: Backspace goes back one level (in the API-key entry
//! it deletes while there is text and only navigates when the input is
//! empty); Esc always closes the whole panel. Providers already signed in
//! are marked in the description column so a return visit shows the result.

use crate::core::auth::{self};
use crate::tui::markdown::visible_width;
use crate::tui::render::{self, bold};
use crate::tui::theme::Theme;

pub enum AuthStage {
    /// The method choice; `selected` indexes the two options. The root:
    /// Esc closes the panel, Backspace has nowhere to go.
    Choose { selected: usize },
    /// Which subscription to sign in with.
    Account { selected: usize },
    /// Which provider the API key belongs to.
    Key { selected: usize },
    /// Key entry for a provider; the composer holds the (masked) secret.
    ApiKey { provider: String },
    /// Browser OAuth in flight. `back` is the account-list row that launched
    /// the flow, or None when it was launched by a direct `/login <provider>`
    /// command (canceling that returns nowhere — the panel closes).
    Waiting { back: Option<usize> },
    /// The flow's outcome beat: shows the result, then any key but Esc
    /// returns to the list the flow belongs to.
    Done {
        ok: bool,
        message: String,
        back: BackTarget,
    },
}

/// Where a finished flow returns: the list it belongs to, selection preserved.
#[derive(Clone, Copy)]
pub enum BackTarget {
    Account(usize),
    Key(usize),
}

impl BackTarget {
    /// The stage this target returns to.
    pub fn stage(self) -> AuthStage {
        match self {
            BackTarget::Account(selected) => AuthStage::Account { selected },
            BackTarget::Key(selected) => AuthStage::Key { selected },
        }
    }
}

const DESCRIPTION_COL: usize = 34;

pub(crate) fn choice_row(
    theme: &Theme,
    selected: bool,
    label: &str,
    description: &str,
    width: usize,
) -> String {
    let caret = if selected { "   › " } else { "     " };
    let head = format!("{caret}{label}");
    let mut row = if selected {
        bold(&theme.fg("userMessageText", &head))
    } else {
        theme.fg("dim", &head)
    };
    if width > DESCRIPTION_COL {
        let pad = DESCRIPTION_COL.saturating_sub(visible_width(&head));
        row.push_str(&theme.fg("dim", &format!("{}{}", " ".repeat(pad), description)));
    }
    row
}

pub fn render(stage: &AuthStage, theme: &Theme, width: usize, mask_count: usize) -> Vec<String> {
    let dim = |s: &str| theme.fg("dim", s);
    match stage {
        AuthStage::Choose { selected } => vec![
            String::new(),
            dim("   Sign in"),
            String::new(),
            choice_row(
                theme,
                *selected == 0,
                "Sign in with an account",
                "subscription — opens the browser",
                width,
            ),
            choice_row(
                theme,
                *selected == 1,
                "Sign in with an API key",
                "stored in ~/.e/auth.json",
                width,
            ),
            String::new(),
            dim("   ↑↓ Choose · Enter Continue · Esc Cancel"),
        ],
        AuthStage::Account { selected } => {
            let mut rows = vec![
                String::new(),
                dim("   Sign in with an account"),
                String::new(),
            ];
            let auth = auth::load();
            for (i, provider) in crate::core::providers::registry::oauth_providers()
                .iter()
                .enumerate()
            {
                let connected = auth::signed_in(&auth, &provider.name);
                let description = if connected {
                    "signed in"
                } else {
                    &provider.auth.oauth_hint
                };
                rows.push(choice_row(
                    theme,
                    *selected == i,
                    &provider.display,
                    description,
                    width,
                ));
            }
            rows.push(String::new());
            rows.push(dim(&format!(
                "   ↑↓ Choose · Enter Continue · {} Back · Esc Close",
                render::backspace_label(),
            )));
            rows
        }
        AuthStage::Key { selected } => {
            let mut rows = vec![
                String::new(),
                dim("   Sign in with an API key"),
                String::new(),
            ];
            let auth = auth::load();
            for (i, provider) in crate::core::providers::registry::key_providers()
                .iter()
                .enumerate()
            {
                let connected = auth::signed_in(&auth, &provider.name);
                let description = if connected {
                    "signed in"
                } else {
                    &provider.auth.key_hint
                };
                rows.push(choice_row(
                    theme,
                    *selected == i,
                    &provider.display,
                    description,
                    width,
                ));
            }
            rows.push(String::new());
            rows.push(dim(&format!(
                "   ↑↓ Choose · Enter Continue · {} Back · Esc Close",
                render::backspace_label(),
            )));
            rows
        }
        AuthStage::ApiKey { provider } => {
            let entry = if mask_count == 0 {
                format!(
                    "{}{}",
                    bold(&theme.fg("userMessageText", "   ┃ ")),
                    dim("Paste or type a key")
                )
            } else {
                bold(&theme.fg(
                    "userMessageText",
                    &format!(
                        "   ┃ {}",
                        "•".repeat(mask_count.min(width.saturating_sub(6)))
                    ),
                ))
            };
            vec![
                String::new(),
                dim(&format!(
                    "   Paste your {} API key",
                    crate::core::providers::catalog::display_name(provider)
                )),
                entry,
                dim(&format!(
                    "   Enter saves · {} Back · Esc Close",
                    render::backspace_label()
                )),
            ]
        }
        AuthStage::Waiting { .. } => vec![
            String::new(),
            dim("   Sign in with an account"),
            String::new(),
            dim("   Waiting for authorization in the browser…"),
            dim(&format!(
                "   {} cancels sign-in · Esc Close",
                render::backspace_label()
            )),
        ],
        AuthStage::Done { ok, message, .. } => {
            let head = if *ok {
                bold(&theme.fg("userMessageText", "   Login successful"))
            } else {
                bold(&theme.fg("userMessageText", "   Sign-in failed"))
            };
            vec![
                String::new(),
                head,
                dim(&format!("   {message}")),
                String::new(),
                dim("   Enter Continue · Esc Close"),
            ]
        }
    }
}
