//! The sign-in panel — the auth surface's own look, distinct from the
//! generic picker: three-space indented prose rows, choices marked with a
//! `› ` caret (the one place a caret appears), descriptions at an absolute
//! column, and per-stage rows. Everything dim except the selected choice or
//! the live input row.
//!
//! Stages: Choose (account vs API key, the reference flow's wording) →
//! Account or Key (which provider, labeled by display name) → ApiKey (inline
//! `┃ •••` entry mirroring the composer) or Waiting (browser authorization
//! in flight).

use crate::tui::markdown::visible_width;
use crate::tui::render::bold;
use crate::tui::theme::Theme;

pub enum AuthStage {
    /// The method choice; `selected` indexes the two options.
    Choose { selected: usize },
    /// Which subscription to sign in with.
    Account { selected: usize },
    /// Which provider the API key belongs to.
    Key { selected: usize },
    /// Key entry for a provider; the composer holds the (masked) secret.
    ApiKey { provider: String },
    /// Browser OAuth in flight.
    Waiting,
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
        AuthStage::Account { selected } => vec![
            String::new(),
            dim("   Sign in with an account"),
            String::new(),
            choice_row(
                theme,
                *selected == 0,
                "OpenAI Codex",
                "ChatGPT — opens the browser",
                width,
            ),
            choice_row(
                theme,
                *selected == 1,
                "xAI",
                "SuperGrok or X Premium — device code",
                width,
            ),
            String::new(),
            dim("   ↑↓ Choose · Enter Continue · Esc Cancel"),
        ],
        AuthStage::Key { selected } => vec![
            String::new(),
            dim("   Sign in with an API key"),
            String::new(),
            choice_row(theme, *selected == 0, "OpenCode Go", "zen", width),
            choice_row(theme, *selected == 1, "xAI", "console.x.ai", width),
            choice_row(
                theme,
                *selected == 2,
                "OpenAI",
                "platform.openai.com",
                width,
            ),
            choice_row(
                theme,
                *selected == 3,
                "Anthropic",
                "console.anthropic.com",
                width,
            ),
            String::new(),
            dim("   ↑↓ Choose · Enter Continue · Esc Cancel"),
        ],
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
                    crate::core::model::display_name(provider)
                )),
                entry,
                dim("   Enter saves · Esc cancels"),
                dim("   Saves to ~/.e/auth.json"),
            ]
        }
        AuthStage::Waiting => vec![
            String::new(),
            dim("   Sign in with an account"),
            String::new(),
            dim("   Waiting for authorization in the browser…"),
            dim("   Esc dismisses this panel; the sign-in continues"),
        ],
    }
}
