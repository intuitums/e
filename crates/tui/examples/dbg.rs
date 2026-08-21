fn main() {
    let t = e_tui::render::theme::Theme::from_json(&std::fs::read_to_string("themes/dark.json").unwrap()).unwrap();
    for l in e_tui::render::markdown::render_markdown(&t, "- one\n  - nested\n\n1. numbered\n", 40) {
        println!("{:?}", l);
    }
}
