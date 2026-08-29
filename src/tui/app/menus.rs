//! The footer menus: commands, models, scoped models, skills, files —
//! building them, syncing them to the composer, and applying a selection.

use super::*;

impl App {
    pub(super) fn command_items(&self) -> Vec<MenuItem> {
        let mut items = vec![
            MenuItem::new(
                "/login",
                "sign in to a provider — account or API key",
                "/login",
            ),
            MenuItem::new("/models", "switch the model", "/models"),
            MenuItem::new(
                "/scoped-models",
                "choose which models ctrl+p cycles",
                "/scoped-models",
            ),
            MenuItem::new(
                "/reload",
                "reload extensions, themes, and config",
                "/reload",
            ),
            MenuItem::new("/resume", "resume a saved session", "/resume"),
            MenuItem::new(
                "/tree",
                "rewind to an earlier point in this session and branch",
                "/tree",
            ),
            MenuItem::new("/new", "start a fresh session", "/new"),
            MenuItem::new("/copy", "copy the last reply", "/copy"),
            MenuItem::new("/compact", "summarize into a fresh session", "/compact"),
            MenuItem::new(
                "/trust",
                "trust this directory (loads its AGENTS.md, .e resources)",
                "/trust",
            ),
            MenuItem::new("/settings", "change preferences", "/settings"),
            MenuItem::new("/help", "show commands", "/help"),
            MenuItem::new("/version", "show the version", "/version"),
            MenuItem::new("/quit", "exit", "/quit"),
        ];
        // Built-in dispatch wins name clashes, so a template or extension
        // command shadowed by a built-in is unreachable — listing it would
        // show a duplicate row that runs the built-in anyway.
        for template in crate::core::resources::prompts::list(&self.agent.cwd()) {
            if is_builtin_command(&template.name) {
                continue;
            }
            let slash = format!("/{}", template.name);
            let description = if template.argument_hint.is_empty() {
                template.description.clone()
            } else {
                format!("{} — {}", template.description, template.argument_hint)
            };
            items.push(MenuItem::new(&slash, &description, &slash));
        }
        for (name, description) in self.host.commands() {
            if is_builtin_command(&name) {
                continue;
            }
            let slash = format!("/{name}");
            items.push(MenuItem::new(&slash, &description, &slash));
        }
        items
    }

    /// The scoped-models multi-select: every available model, Space toggling
    /// membership. The reference semantics: no scope stored = everything in
    /// scope; the first toggle narrows the scope to just that model.
    pub(super) fn open_scoped_menu(&mut self) {
        let available = model::available();
        if available.is_empty() {
            self.notice("no models available — use /login to sign in to a provider".into());
            return;
        }
        let scope = model::scope();
        let mut available = model::provider_grouped(available);
        // The scoped entries lead the list — what you curated, not a hunt.
        if let Some(ids) = &scope {
            available.sort_by_key(|m| !ids.contains(&model::slug(m)));
        }
        let items: Vec<MenuItem> = available
            .iter()
            .map(|m| {
                let slug = model::slug(m);
                let mut item = MenuItem::new(&m.id, &m.provider, &slug);
                let in_scope = match &scope {
                    Some(ids) => ids.contains(&slug),
                    None => true,
                };
                if in_scope {
                    item.meta = "in scope".into();
                }
                item
            })
            .collect();
        self.menu = Some(Menu::new(
            MenuKind::Scoped,
            "Scoped models",
            HINT_SCOPED,
            items,
        ));
    }

    /// Space on the scoped picker: reference toggle semantics — no scope yet
    /// means the first toggle starts a scope of exactly that model.
    pub(super) fn toggle_scoped(&mut self) {
        let Some(slug) = self
            .menu
            .as_ref()
            .and_then(|menu| menu.current().map(|item| item.value.clone()))
        else {
            return;
        };
        match model::scope() {
            None => {
                if let Err(error) = model::set_scope(std::slice::from_ref(&slug)) {
                    self.notice(format!("could not save model scope: {error}"));
                    return;
                }
                if let Some(menu) = &mut self.menu {
                    menu.for_each_item(|item| {
                        item.meta = if item.value == slug {
                            "in scope".into()
                        } else {
                            String::new()
                        };
                    });
                }
            }
            Some(mut ids) => {
                let meta = if let Some(pos) = ids.iter().position(|id| *id == slug) {
                    ids.remove(pos);
                    ""
                } else {
                    ids.push(slug.clone());
                    "in scope"
                };
                if let Err(error) = model::set_scope(&ids) {
                    self.notice(format!("could not save model scope: {error}"));
                    return;
                }
                if let Some(item) = self.menu.as_mut().and_then(|menu| menu.current_mut()) {
                    item.meta = meta.into();
                }
            }
        }
    }

    pub(super) fn open_model_menu(&mut self) {
        // Instant discovery where it matters: show the cached list now, ask
        // the gateways in the background (60s floor), and pop new rows into
        // the open picker when the answer lands.
        let results = self.results.clone();
        tokio::spawn(async move {
            crate::core::providers::catalog::refresh_remote_within(60_000).await;
            let _ = results.send(AppJob::CatalogRefreshed).await;
        });
        self.build_model_menu();
    }

    /// The picker itself, from the current catalog — no refresh side effects,
    /// so the rebuild-on-refresh arm cannot loop.
    pub(super) fn build_model_menu(&mut self) {
        /// `200K context · 8K output` — exact multiples compact to K/M,
        /// anything else stays raw, the reference's fact grammar.
        fn model_facts(m: &crate::core::providers::catalog::Model) -> String {
            fn token_fact(tokens: u64, suffix: &str) -> String {
                if tokens >= 1_000_000 && tokens.is_multiple_of(1_000_000) {
                    format!("{}M {suffix}", tokens / 1_000_000)
                } else if tokens >= 1_000 && tokens.is_multiple_of(1_000) {
                    format!("{}K {suffix}", tokens / 1_000)
                } else {
                    format!("{tokens} {suffix}")
                }
            }
            let mut facts = Vec::new();
            if m.context_window > 0 {
                facts.push(token_fact(m.context_window, "context"));
            }
            if let Some(output) = m.max_output {
                facts.push(token_fact(output, "output"));
            }
            facts.join(" · ")
        }
        let available = model::provider_grouped(model::available());
        if available.is_empty() {
            self.notice("no models available — use /login to sign in to a provider".into());
            return;
        }
        let current = self.agent.model_slug();
        // Provider tabs: All, then each provider with models available, in
        // the grouped order the rows themselves use.
        let mut tabs = vec!["All".to_string()];
        for m in &available {
            let display = crate::core::providers::catalog::display_name(&m.provider);
            if !tabs[1..].contains(&display) {
                tabs.push(display);
            }
        }
        // The reference's model rows carry a dim compact-facts column —
        // `200K context · 8K output` — two columns past the longest id; the
        // current model is where the selection starts, not a marker.
        let items = available
            .iter()
            .map(|m| {
                let mut item = MenuItem::new(&m.id, &model_facts(m), &model::slug(m));
                let display = crate::core::providers::catalog::display_name(&m.provider);
                item.tab = tabs.iter().position(|t| *t == display);
                item
            })
            .collect();
        let mut menu = Menu::new(MenuKind::Models, "Models", HINT_MODELS, items).with_tabs(
            tabs,
            Some(0),
            0,
            "",
        );
        menu.select_value(&current);
        self.menu = Some(menu);
    }

    pub(super) fn open_skills_menu(&mut self, query: &str) {
        // Single-line rows, the reference's grammar: the skill name with a
        // dim source scope beside it, no description — Tab cycles the
        // source filter.
        let global_root = crate::core::config::home::skills_dir();
        let items: Vec<MenuItem> = crate::core::resources::skills::list(&self.agent.cwd())
            .into_iter()
            .map(|s| {
                let global = s.dir.starts_with(&global_root);
                let scope = if global { "Global" } else { "Workspace" };
                let mut item = MenuItem::new(&s.name, scope, &s.name);
                item.tab = Some(if global { 1 } else { 2 });
                item
            })
            .collect();
        if items.is_empty() {
            return;
        }
        let mut menu = Menu::new(MenuKind::Skills, "Skills", HINT_SKILLS, items).with_tabs(
            vec!["All".into(), "Global".into(), "Workspace".into()],
            Some(0),
            0,
            "Source",
        );
        menu.set_query(query);
        self.menu = Some(menu);
    }

    pub(super) fn open_file_menu(&mut self, query: &str) {
        let cwd = std::env::current_dir().unwrap_or_default();
        let items = crate::core::workspace::list_files(&cwd)
            .into_iter()
            .map(|path| MenuItem::new(&path, "", &path))
            .collect();
        let mut menu = Menu::new(MenuKind::Files, "Files", HINT_USE, items);
        menu.set_query(query);
        self.menu = Some(menu);
    }

    /// Keep pickers in sync with the composer text: `/` at the start opens
    /// the command picker, an `@word` under the cursor the file picker.
    pub(super) fn sync_menu(&mut self) {
        let text = self.editor.text();
        if self.pending_key.is_some() {
            return;
        }
        // /help opens this picker after its slash command has already left the
        // composer. In that mode all later composer input is the query.
        if let Some(menu) = self
            .menu
            .as_mut()
            .filter(|menu| menu.kind == MenuKind::Commands && menu.filter_without_trigger)
        {
            menu.set_query(text.strip_prefix('/').unwrap_or(&text));
            return;
        }
        // Slash picker: leading '/', no space yet.
        if text.starts_with('/') && !text.contains(' ') && !text.contains('\n') {
            let query = text[1..].to_string();
            match &mut self.menu {
                Some(m) if m.kind == MenuKind::Commands => m.set_query(&query),
                _ => {
                    let mut menu = Menu::new(
                        MenuKind::Commands,
                        "Commands",
                        HINT_USE,
                        self.command_items(),
                    );
                    menu.set_query(&query);
                    self.menu = Some(menu);
                }
            }
            return;
        }
        // File picker: the last token starts with '@'.
        if let Some(token) = text
            .split_whitespace()
            .last()
            .filter(|t| t.starts_with('@'))
        {
            let query = token[1..].to_string();
            match &mut self.menu {
                Some(m) if m.kind == MenuKind::Files => m.set_query(&query),
                _ => self.open_file_menu(&query),
            }
            return;
        }
        // Skills picker: the last token starts with '$'.
        if let Some(token) = text
            .split_whitespace()
            .last()
            .filter(|t| t.starts_with('$'))
        {
            let query = token[1..].to_string();
            match &mut self.menu {
                Some(m) if m.kind == MenuKind::Skills => m.set_query(&query),
                _ => self.open_skills_menu(&query),
            }
            return;
        }
        // Auto pickers close when their trigger text is gone.
        if matches!(
            self.menu.as_ref().map(|m| m.kind),
            Some(MenuKind::Commands) | Some(MenuKind::Files) | Some(MenuKind::Skills)
        ) {
            self.menu = None;
        }
    }

    /// Enter on an open picker. Returns true when the key was consumed.
    pub(super) fn select_menu(&mut self) -> bool {
        let Some(menu) = &self.menu else { return false };
        let Some(item) = menu.current().cloned() else {
            self.menu = None;
            return true;
        };
        let kind = menu.kind;
        self.menu = None;
        match kind {
            MenuKind::Commands => {
                self.editor.set_text("");
                self.dispatch_command(item.value);
            }
            MenuKind::Files => {
                // Replace the @token under construction with the chosen path.
                let text = self.editor.text();
                let replaced = match text.rfind('@') {
                    Some(at) => format!("{}{}", &text[..at], item.value),
                    None => item.value,
                };
                self.editor.set_text(&replaced);
            }
            MenuKind::Sessions => {
                self.resume_path(std::path::PathBuf::from(item.value));
            }
            MenuKind::Scoped => {}
            MenuKind::Skills => {
                // Replace the $token, then send the skill body as context.
                let text = self.editor.text();
                let rest = match text.rfind('$') {
                    Some(at) => text[..at].trim_end().to_string(),
                    None => String::new(),
                };
                self.editor.set_text("");
                if let Some(skill) =
                    crate::core::resources::skills::get(&item.value, &self.agent.cwd())
                {
                    // The directory rides along, exactly as the system-prompt
                    // catalog carries it: a body that says "see reference.md"
                    // strands the model without the path it lives at.
                    let body = format!(
                        "{}\n\n[skill directory: {} — files this skill references live there]",
                        skill.body,
                        skill.dir.display()
                    );
                    let combined = if rest.is_empty() {
                        body
                    } else {
                        format!("{body}\n\n{rest}")
                    };
                    self.prompt(combined);
                }
            }
            MenuKind::Models => {
                if let Some(found) = model::resolve(&item.value) {
                    if let Err(error) = persist_model(&found) {
                        self.notice(format!("could not save model choice: {error}"));
                        return true;
                    }
                    self.notice(format!("model set to {}", model::slug(&found)));
                    self.agent.model = found;
                    self.refresh_status_cache();
                }
            }
            MenuKind::Tree => {
                self.rewind_to_node(&item.value);
            }
        }
        true
    }
}
