//! Prompt templates. Pins the bash-style substitution contract and the
//! frontmatter parse — the pieces a user's template actually relies on.

use e::core::resources::prompts::substitute;

#[test]
fn positional_and_all_args() {
    assert_eq!(
        substitute("fix $1 in $2", "bug parser.rs"),
        "fix bug in parser.rs"
    );
    assert_eq!(substitute("review: $@", "a b c"), "review: a b c");
    assert_eq!(substitute("review: $ARGUMENTS", "a b"), "review: a b");
    assert_eq!(substitute("missing [$3]", "one two"), "missing []");
}

#[test]
fn defaults_and_slices() {
    assert_eq!(substitute("${1:-all files}", ""), "all files");
    assert_eq!(substitute("${1:-all files}", "src/"), "src/");
    assert_eq!(substitute("${@:-nothing}", ""), "nothing");
    assert_eq!(
        substitute("rest: ${@:2}", "first second third"),
        "rest: second third"
    );
}

#[test]
fn quotes_group_words() {
    assert_eq!(
        substitute("[$1] [$2]", r#""two words" three"#),
        "[two words] [three]"
    );
    assert_eq!(substitute("[$1]", "'a b c'"), "[a b c]");
}

#[test]
fn dollars_without_a_meaning_stay_literal() {
    // $5 is positional (bash-style) and expands empty without args…
    assert_eq!(substitute("costs $5, ok?", ""), "costs , ok?");
    // …but a dollar before a plain word is just a dollar.
    assert_eq!(substitute("$notavar", ""), "$notavar");
}
