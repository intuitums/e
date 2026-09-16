//! The built-in documentation, embedded in the binary and served by
//! `e docs [topic]` — a single binary has no package directory to point at,
//! so the binary itself is the docs carrier. The system prompt tells the
//! agent to run it when asked about e's own surfaces.
//!
//! The topics are not listed here: `build.rs` generates them from `docs/`,
//! where the folder is the nav group, the file stem is the topic, and front
//! matter carries the blurb. The website renders the same files, so a guide is
//! written once and read by the terminal, GitHub, and the site.

mod generated {
    include!(concat!(env!("OUT_DIR"), "/docs.rs"));
}

pub use generated::TOPICS;

/// The bundled themes are assets rather than guides, so they are listed beside
/// the guides the folders provide.
const THEME_TOPICS: &[(&str, &str)] = &[
    (
        "theme-dark",
        "the built-in dark theme, verbatim (a starting point)",
    ),
    ("theme-light", "the built-in light theme, verbatim"),
];

/// Every topic `e docs` serves, in the order the folders and front matter give.
pub fn topics() -> impl Iterator<Item = (&'static str, &'static str)> {
    TOPICS.iter().copied().chain(THEME_TOPICS.iter().copied())
}

/// One topic's text, without the front matter that labels it on the website.
pub fn body(topic: &str) -> Option<&'static str> {
    let raw = match topic {
        "theme-dark" => include_str!("../../../assets/themes/dark.json"),
        "theme-light" => include_str!("../../../assets/themes/light.json"),
        _ => generated::body(topic)?,
    };
    Some(strip_front_matter(raw))
}

/// Front matter is the website's metadata; a terminal prints the prose.
fn strip_front_matter(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("---\n") else {
        return text;
    };
    match rest.find("\n---\n") {
        Some(end) => rest[end + 5..].trim_start_matches('\n'),
        None => text,
    }
}
