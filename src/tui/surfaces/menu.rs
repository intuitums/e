//! The footer picker band.
//!
//! Menus render between the composer and the status row, the reference
//! inline-picker shape:
//!
//! ```text
//!   ── divider ─────────────────────────────
//!   Commands 7 · Type to filter          1–7
//!
//!     /login   sign in to a provider
//!     /model   list or switch models       ← selected row bright, no caret
//!   ── divider ─────────────────────────────
//!   ↑↓ Navigate     Enter Use     Esc Close   (rides the status row)
//! ```
//!
//! Selection is brightness alone on every surface — bold bright ink for the
//! current row, the rest dim, no fill and no caret. The band holds only its
//! rows: at most MAX_VISIBLE, shrinking with a short match list instead of
//! blank-padding.

use unicode_width::UnicodeWidthChar;

use crate::tui::markdown::visible_width;
use crate::tui::theme::Theme;

#[derive(Clone)]
pub struct MenuItem {
    pub label: String,
    pub description: String,
    /// Right-aligned dim metadata.
    pub meta: String,
    pub value: String,
    /// The tab this item belongs to, when the menu has tabs; None shows
    /// under every tab.
    pub tab: Option<usize>,
}

impl MenuItem {
    pub fn new(label: &str, description: &str, value: &str) -> Self {
        MenuItem {
            label: label.into(),
            description: description.into(),
            meta: String::new(),
            value: value.into(),
            tab: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    Commands,
    Files,
    Models,
    Sessions,
    Skills,
    /// The scoped-models multi-select: Space toggles, Enter closes.
    Scoped,
    /// /tree: pick an earlier point in this session to rewind to.
    Tree,
}

pub struct Menu {
    pub kind: MenuKind,
    pub title: &'static str,
    pub hint: &'static str,
    items: Vec<MenuItem>,
    filtered: Vec<usize>,
    pub selected: usize,
    window_start: usize,
    query: String,
    /// True when a command opened this picker without trigger text in the
    /// composer, so subsequent composer input is the filter itself.
    pub(crate) filter_without_trigger: bool,
    /// Tab labels, cycled with the Tab key; empty for tab-less pickers.
    tabs: Vec<String>,
    active_tab: usize,
    /// The tab that shows everything (the "All" tab), when one exists.
    all_tab: Option<usize>,
    /// The compact header's noun for the tab dimension ("Source",
    /// "Provider", "Scope").
    tab_noun: &'static str,
}

pub const HINT_USE: &str = "↑↓ Navigate     Enter Use     Esc Close";
pub const HINT_SCOPED: &str =
    "↑↓ Navigate     Space Toggle     Ctrl+X Reset     Enter Done     Esc Close";
pub const HINT_SKILLS: &str = "↑↓ Navigate     Tab Source     Enter Use     Esc Close";
pub const HINT_MODELS: &str = "↑↓ Navigate     Tab Provider     Enter Use     Esc Close";
pub const HINT_SESSIONS: &str = "↑↓ Navigate     Tab Scope     Enter Resume     Esc Close";
/// The reference keeps six selectable rows below the header — except the
/// model picker, which shows up to twenty.
const MAX_VISIBLE: usize = 6;
const MAX_VISIBLE_MODELS: usize = 20;

/// The char indices `fuzzy_score`'s walk would hit — the spans the picker
/// brightens so a filtered row shows why it matched. None when the query is
/// not a subsequence of the candidate.
pub fn fuzzy_positions(query: &str, candidate: &str) -> Option<Vec<usize>> {
    if query.is_empty() {
        return Some(Vec::new());
    }
    let lower = |c: char| c.to_lowercase().next().unwrap_or(c);
    let chars: Vec<char> = candidate.chars().map(lower).collect();
    let mut positions = Vec::with_capacity(query.chars().count());
    let mut from = 0usize;
    for needle in query.chars().map(lower) {
        let at = chars[from..].iter().position(|&c| c == needle)? + from;
        positions.push(at);
        from = at + 1;
    }
    Some(positions)
}

/// Subsequence fuzzy match; lower score is better, None is no match.
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(usize::MAX / 2); // neutral: keep original order
    }
    let candidate_lower = candidate.to_lowercase();
    let query_lower = query.to_lowercase();
    let mut score = 0usize;
    let mut position = 0usize;
    let mut last_hit: Option<usize> = None;
    for needle in query_lower.chars() {
        let found = candidate_lower[position..].find(needle)?;
        let at = position + found;
        // Contiguity bonus: gaps cost, adjacency is free.
        if let Some(last) = last_hit {
            score += at - last - 1;
        } else {
            score += at; // earlier first hits rank higher
        }
        last_hit = Some(at);
        position = at + needle.len_utf8();
    }
    Some(score)
}

impl Menu {
    pub fn new(
        kind: MenuKind,
        title: &'static str,
        hint: &'static str,
        items: Vec<MenuItem>,
    ) -> Self {
        let mut menu = Menu {
            kind,
            title,
            hint,
            items,
            filtered: Vec::new(),
            selected: 0,
            window_start: 0,
            query: String::new(),
            filter_without_trigger: false,
            tabs: Vec::new(),
            active_tab: 0,
            all_tab: None,
            tab_noun: "",
        };
        menu.refilter();
        menu
    }

    /// Keep filtering from composer text when no `/` trigger remains.
    pub fn without_trigger(mut self) -> Self {
        self.filter_without_trigger = true;
        self
    }

    /// Give the picker a Tab-cycled filter dimension: labels, which one
    /// shows everything, the tab active on open, and the compact header's
    /// noun for it (empty for pickers whose narrow header skips the noun).
    pub fn with_tabs(
        mut self,
        tabs: Vec<String>,
        all_tab: Option<usize>,
        active: usize,
        tab_noun: &'static str,
    ) -> Self {
        self.tabs = tabs;
        self.all_tab = all_tab;
        self.active_tab = active;
        self.tab_noun = tab_noun;
        self.refilter();
        self
    }

    pub fn has_tabs(&self) -> bool {
        !self.tabs.is_empty()
    }

    /// Cycle to the next tab, wrapping. A no-op for tab-less pickers.
    pub fn cycle_tab(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        self.active_tab = (self.active_tab + 1) % self.tabs.len();
        self.refilter();
    }

    /// Cycle to the previous tab, wrapping. A no-op for tab-less pickers.
    pub fn cycle_tab_back(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        self.active_tab = (self.active_tab + self.tabs.len() - 1) % self.tabs.len();
        self.refilter();
    }

    fn tab_admits(&self, item: &MenuItem) -> bool {
        self.tabs.is_empty()
            || Some(self.active_tab) == self.all_tab
            || item.tab.is_none()
            || item.tab == Some(self.active_tab)
    }

    pub fn set_query(&mut self, query: &str) {
        if self.query != query {
            self.query = query.to_string();
            self.refilter();
        }
    }

    fn refilter(&mut self) {
        let mut scored: Vec<(usize, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| self.tab_admits(item))
            .filter_map(|(i, item)| {
                let best = fuzzy_score(&self.query, &item.label)
                    .into_iter()
                    .chain(fuzzy_score(&self.query, &item.description))
                    .min()?;
                Some((best, i))
            })
            .collect();
        scored.sort_by_key(|(score, i)| (*score, *i));
        self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        self.selected = 0;
        self.window_start = 0;
    }

    pub fn len(&self) -> usize {
        self.filtered.len()
    }

    pub fn is_empty(&self) -> bool {
        self.filtered.is_empty()
    }

    /// Move the selection to the item with this value, if present.
    pub fn select_value(&mut self, value: &str) {
        if let Some(pos) = self
            .filtered
            .iter()
            .position(|&i| self.items[i].value == value)
        {
            self.selected = pos;
        }
    }

    pub fn for_each_item(&mut self, mut f: impl FnMut(&mut MenuItem)) {
        for item in &mut self.items {
            f(item);
        }
    }

    pub fn current_mut(&mut self) -> Option<&mut MenuItem> {
        let idx = *self.filtered.get(self.selected)?;
        self.items.get_mut(idx)
    }

    pub fn current(&self) -> Option<&MenuItem> {
        self.filtered.get(self.selected).map(|&i| &self.items[i])
    }

    /// Selectable rows below the header: twenty for the model picker, six
    /// everywhere else — the reference's budgets.
    fn max_visible(&self) -> usize {
        match self.kind {
            MenuKind::Models => MAX_VISIBLE_MODELS,
            _ => MAX_VISIBLE,
        }
    }

    pub fn step(&mut self, delta: isize) {
        let n = self.filtered.len();
        if n == 0 {
            return;
        }
        let visible = self.max_visible();
        self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        if self.selected < self.window_start {
            self.window_start = self.selected;
        }
        if self.selected >= self.window_start + visible {
            self.window_start = self.selected - visible + 1;
        }
    }

    /// What an empty result set says, in the reference's own words per
    /// surface — the skills picker names the active source tab when a
    /// narrower filter came up empty.
    fn empty_notice(&self) -> String {
        if self.kind == MenuKind::Skills
            && !self.tabs.is_empty()
            && Some(self.active_tab) != self.all_tab
        {
            return format!("No {} skills found.", self.tabs[self.active_tab]);
        }
        match self.kind {
            MenuKind::Commands => "no matching slash commands",
            MenuKind::Files => "no matching files",
            MenuKind::Skills => "No skills found.",
            MenuKind::Models => "No models found.",
            MenuKind::Sessions => "No sessions found.",
            _ => "Nothing found.",
        }
        .to_string()
    }

    /// The row's dress as an open/close pair, so matched spans can reset
    /// and reopen mid-run. Every picker signals selection by brightness
    /// alone — bold bright ink for the current row — and leaves unselected
    /// rows dim; no picker fills the row, and there is no caret either way.
    fn row_style(&self, theme: &Theme, selected: bool) -> (String, String) {
        if !selected {
            let open = theme.fg_prefix("dim").to_string();
            let close = if open.is_empty() { "" } else { "\x1b[39m" };
            return (open, close.into());
        }
        let fg = theme.fg_prefix("userMessageText");
        let close = if fg.is_empty() {
            "\x1b[22m"
        } else {
            "\x1b[39m\x1b[22m"
        };
        (format!("\x1b[1m{fg}"), close.to_string())
    }

    /// One tab, dressed: the active tab `[bracketed]` in the bright style,
    /// the rest dim.
    fn tab_label(&self, theme: &Theme, index: usize) -> String {
        let label = &self.tabs[index];
        if index == self.active_tab {
            crate::tui::render::bold(&theme.fg("userMessageText", &format!("[{label}]")))
        } else {
            theme.fg("dim", label)
        }
    }

    fn tabbed_header(&self, theme: &Theme, width: usize, count: usize) -> String {
        let title = crate::tui::render::bold(
            &theme.fg("userMessageText", &format!("{} {count}", self.title)),
        );
        // Widest form: title, then every tab two spaces apart.
        let mut wide = title.clone();
        for index in 0..self.tabs.len() {
            wide.push_str("  ");
            wide.push_str(&self.tab_label(theme, index));
        }
        if visible_width(&wide) <= width {
            return wide;
        }
        let active = self.tab_label(theme, self.active_tab);
        if !self.tab_noun.is_empty() {
            // The noun ladder: title, the dimmed tab noun, the active tab
            // — then title and tab, then the active tab alone.
            let compact = format!(
                "{title}    {}{active}",
                theme.fg("dim", &format!("{} ", self.tab_noun))
            );
            if visible_width(&compact) <= width {
                return compact;
            }
            let tight = format!("{title}  {active}");
            if visible_width(&tight) <= width {
                return tight;
            }
            return active;
        }
        if self.kind == MenuKind::Models {
            return self.windowed_tabs(theme, width, &title);
        }
        // Noun-less pickers end their ladder at title plus the active tab.
        format!("{title}  {active}")
    }

    /// The model picker's tab window: grow the visible run of tabs around
    /// the active one while room remains, marking a clipped end with a dim
    /// ellipsis.
    fn windowed_tabs(&self, theme: &Theme, width: usize, title: &str) -> String {
        let title_width = visible_width(title);
        let tab_width = |index: usize| {
            visible_width(&self.tabs[index]) + if index == self.active_tab { 2 } else { 0 }
        };
        if title_width + 2 + tab_width(self.active_tab) > width {
            return format!("{title}  {}", self.tab_label(theme, self.active_tab));
        }
        let range_width = |start: usize, end: usize| {
            let mut w = if start > 0 { 3 } else { 0 };
            for position in start..end {
                if position > start {
                    w += 2;
                }
                w += tab_width(position);
            }
            if end < self.tabs.len() {
                w += 3;
            }
            w
        };
        let mut start = self.active_tab;
        let mut end = self.active_tab + 1;
        loop {
            let mut expanded = false;
            if end < self.tabs.len() && title_width + 2 + range_width(start, end + 1) <= width {
                end += 1;
                expanded = true;
            }
            if start > 0 && title_width + 2 + range_width(start - 1, end) <= width {
                start -= 1;
                expanded = true;
            }
            if !expanded {
                break;
            }
        }
        let mut row = format!("{title}  ");
        if start > 0 {
            row.push_str(&theme.fg("dim", "…"));
            row.push_str("  ");
        }
        for position in start..end {
            if position > start {
                row.push_str("  ");
            }
            row.push_str(&self.tab_label(theme, position));
        }
        if end < self.tabs.len() {
            row.push_str("  ");
            row.push_str(&theme.fg("dim", "…"));
        }
        row
    }

    pub fn render(&self, theme: &Theme, width: usize) -> Vec<String> {
        let count = self.filtered.len();
        let visible = self.max_visible();
        let window_end = (self.window_start + visible).min(count);

        // A tab-less header is uniformly dim — `Commands 7 · Type to
        // filter` before the first keystroke, the bare noun and count once
        // a filter is live (the composer already shows the typed query). A
        // tabbed header brightens its title and lays the tabs out two
        // spaces apart, the active one `[bracketed]`, degrading to a
        // `{noun} [active]` compact form and finally the active tab alone.
        let header = if self.tabs.is_empty() {
            let mut header_plain = if self.query.is_empty() {
                format!("{} {count} · Type to filter", self.title)
            } else {
                format!("{} {count}", self.title)
            };
            if count > visible {
                // Scroll range, right-aligned with the reference's
                // one-column margin.
                let range = format!("{}–{}", self.window_start + 1, window_end);
                let pad =
                    width.saturating_sub(1 + visible_width(&header_plain) + range.chars().count());
                if pad > 1 {
                    header_plain.push_str(&" ".repeat(pad));
                    header_plain.push_str(&range);
                }
            }
            theme.fg("dim", &header_plain)
        } else {
            self.tabbed_header(theme, width, count)
        };

        let mut body: Vec<String> = Vec::new();
        if count == 0 {
            body.push(theme.fg("dim", &format!("  {}", self.empty_notice())));
        }
        let label_width = self.filtered[self.window_start..window_end]
            .iter()
            .map(|&i| visible_width(&self.items[i].label))
            .max()
            .unwrap_or(0)
            .min(36);
        // The sessions rows lay their `workspace · age · N turns` cluster
        // in shared fixed columns past the longest title — the reference's
        // self-sizing metadata layout.
        let session_columns = (self.kind == MenuKind::Sessions).then(|| {
            let mut cols = (0usize, 0usize, 0usize, 0usize);
            for &i in &self.filtered {
                let item = &self.items[i];
                let (workspace, age, turns) = split_session_meta(&item.meta);
                cols.0 = cols.0.max(visible_width(&item.label));
                cols.1 = cols.1.max(visible_width(workspace));
                cols.2 = cols.2.max(visible_width(age));
                cols.3 = cols.3.max(visible_width(turns));
            }
            cols
        });
        for slot in self.window_start..window_end {
            let item = &self.items[self.filtered[slot]];
            let selected = slot == self.selected;
            let (open, close) = self.row_style(theme, selected);
            if let Some((title_max, ws_col, age_col, turns_col)) = session_columns {
                // Title middle-ellipsized into its column; workspace and
                // turns left-aligned, age right-aligned; metadata hides
                // entirely when the title would drop below twelve cells.
                let (workspace, age, turns) = split_session_meta(&item.meta);
                let meta_width = if ws_col + age_col + turns_col == 0 {
                    0
                } else {
                    ws_col + 3 + age_col + 3 + turns_col
                };
                let content_width = width.saturating_sub(1);
                let available_title = content_width.saturating_sub(2 + 4 + meta_width);
                let show_meta = meta_width > 0 && available_title >= 12;
                let measured = title_max.max(visible_width(&item.label));
                let title_budget = if show_meta {
                    measured.min(available_title)
                } else {
                    width.saturating_sub(2)
                };
                let title = middle_ellipsize(&item.label, title_budget);
                let mut content = title.clone();
                if show_meta {
                    let meta_start = title_budget + 4;
                    content.push_str(&" ".repeat(meta_start.saturating_sub(visible_width(&title))));
                    content.push_str(workspace);
                    content.push_str(&" ".repeat(ws_col - visible_width(workspace)));
                    content.push_str(" · ");
                    content.push_str(&" ".repeat(age_col - visible_width(age)));
                    content.push_str(age);
                    content.push_str(" · ");
                    content.push_str(turns);
                }
                body.push(crate::tui::markdown::clip_styled(
                    &format!("  {open}{content}{close}"),
                    width,
                ));
                continue;
            }
            // The label, with the query's matched chars brightened: each hit
            // resets to bold and reopens the row's own dress after — the
            // reference's way of showing why a filtered row matched.
            let marks = if self.query.is_empty() {
                Vec::new()
            } else {
                fuzzy_positions(&self.query, &item.label).unwrap_or_default()
            };
            // File rows project through the reference's path segmentation;
            // every other picker shows its label whole. Source indices ride
            // along so match marks survive an ellipsized projection.
            let projected: Vec<(char, Option<usize>)> = if self.kind == MenuKind::Files {
                project_path(&item.label, width.saturating_sub(3))
            } else {
                item.label
                    .chars()
                    .enumerate()
                    .map(|(i, c)| (c, Some(i)))
                    .collect()
            };
            let mut content = String::new();
            for (c, source) in &projected {
                if source.map(|i| marks.contains(&i)).unwrap_or(false) {
                    content.push_str(&format!("\x1b[1m{c}\x1b[0m{open}"));
                } else {
                    content.push(*c);
                }
            }
            let shown_width: usize = projected.iter().map(|(c, _)| c.width().unwrap_or(0)).sum();
            content.push_str(&" ".repeat(label_width.saturating_sub(shown_width)));
            if !item.description.is_empty() {
                // The model picker's facts column sits two spaces past the
                // longest id, the skills picker's source scope four — the
                // reference's inline column gap; every other picker keeps
                // the three-space gap.
                let gap = match self.kind {
                    MenuKind::Models => "  ",
                    MenuKind::Skills => "    ",
                    _ => "   ",
                };
                content.push_str(gap);
                content.push_str(&item.description);
            }
            if !item.meta.is_empty() {
                let pad = width
                    .saturating_sub(1 + 2 + visible_width(&content) + visible_width(&item.meta));
                if pad > 3 {
                    content.push_str(&" ".repeat(pad));
                    content.push_str(&item.meta);
                }
            }
            // Escape-aware clipping: SGR runs are zero columns and a cut row
            // closes its styles instead of severing a sequence mid-run.
            body.push(crate::tui::markdown::clip_styled(
                &format!("  {open}{content}{close}"),
                width,
            ));
        }
        // The band holds only its rows — the reference never blank-pads a
        // short match list.
        crate::tui::panel::frame(theme, width, header, body)
    }
}

/// A session row's `workspace · age · N turns` cluster, split back into
/// its columns (empty strings when the meta is absent or malformed).
fn split_session_meta(meta: &str) -> (&str, &str, &str) {
    let mut parts = meta.rsplitn(3, " · ");
    let turns = parts.next().unwrap_or("");
    let age = parts.next().unwrap_or("");
    let workspace = parts.next().unwrap_or("");
    (workspace, age, turns)
}

/// Middle-ellipsize into `budget` display cells: the head keeps the
/// larger half, `…` bridges to the tail.
fn middle_ellipsize(text: &str, budget: usize) -> String {
    if visible_width(text) <= budget {
        return text.to_string();
    }
    if budget == 0 {
        return String::new();
    }
    if budget == 1 {
        return "…".into();
    }
    let chars: Vec<char> = text.chars().collect();
    let cell = |c: &char| c.width().unwrap_or(0);
    let content = budget - 1;
    let (front, back) = (content.div_ceil(2), content / 2);
    let mut used = 0;
    let mut head = 0;
    while head < chars.len() && used + cell(&chars[head]) <= front {
        used += cell(&chars[head]);
        head += 1;
    }
    used = 0;
    let mut tail = chars.len();
    while tail > head && used + cell(&chars[tail - 1]) <= back {
        used += cell(&chars[tail - 1]);
        tail -= 1;
    }
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[tail..]);
    out
}

/// The file picker's path projection, the reference's segmentation: the
/// whole path when it fits; otherwise the dirname middle-ellipsized into a
/// narrow fixed budget (a third of the row, clamped to 3–12 cells) and the
/// basename prefix-biased, so the distinguishing tail survives. Directories
/// keep a trailing slash. Each projected char carries its source index so
/// match marks survive the ellipses (which carry None).
pub fn project_path(label: &str, width: usize) -> Vec<(char, Option<usize>)> {
    let chars: Vec<char> = label.chars().collect();
    let is_dir = label.ends_with('/');
    let path_len = if is_dir { chars.len() - 1 } else { chars.len() };
    let path = &chars[..path_len];
    let cell = |c: &char| c.width().unwrap_or(0);
    let vw = |s: &[char]| s.iter().map(cell).sum::<usize>();
    let slash_width = usize::from(is_dir);
    let indexed = |range: std::ops::Range<usize>| -> Vec<(char, Option<usize>)> {
        range.map(|i| (chars[i], Some(i))).collect()
    };
    // Prefix/suffix of a char range by display width.
    let prefix_by = |start: usize, end: usize, budget: usize| -> usize {
        let mut used = 0;
        let mut taken = start;
        while taken < end && used + cell(&chars[taken]) <= budget {
            used += cell(&chars[taken]);
            taken += 1;
        }
        taken - start
    };
    let suffix_by = |start: usize, end: usize, budget: usize| -> usize {
        let mut used = 0;
        let mut taken = end;
        while taken > start && used + cell(&chars[taken - 1]) <= budget {
            used += cell(&chars[taken - 1]);
            taken -= 1;
        }
        end - taken
    };
    // One segment, ellipsized to `budget`: middle placement splits evenly,
    // prefix-biased keeps three quarters of the head.
    let ellipsized = |start: usize, end: usize, budget: usize, middle: bool| {
        let mut out: Vec<(char, Option<usize>)> = Vec::new();
        if budget == 0 || start >= end {
            return out;
        }
        if vw(&chars[start..end]) <= budget {
            return indexed(start..end);
        }
        if budget == 1 {
            out.push(('…', None));
            return out;
        }
        let content = budget - 1;
        let (front, back) = if middle {
            (content.div_ceil(2), content / 2)
        } else {
            (content - content / 4, content / 4)
        };
        let head = prefix_by(start, end, front);
        let tail = suffix_by(start, end, back);
        out.extend(indexed(start..start + head));
        out.push(('…', None));
        out.extend(indexed(end - tail..end));
        out
    };
    let with_slash = |mut out: Vec<(char, Option<usize>)>| {
        if is_dir {
            out.push(('/', Some(path_len)));
        }
        out
    };
    if vw(path) + slash_width <= width {
        return with_slash(indexed(0..path_len));
    }
    let basename_start = path
        .iter()
        .rposition(|&c| c == '/')
        .map(|i| i + 1)
        .unwrap_or(0);
    // No directory, or a row too narrow to segment: basename alone,
    // prefix-biased (directories keep one cell for their slash).
    if basename_start == 0 || width < 8 {
        let budget = width.saturating_sub(slash_width);
        return with_slash(ellipsized(basename_start, path_len, budget, false));
    }
    let dirname_end = basename_start - 1;
    let directory_budget = vw(&chars[..dirname_end]).min((width / 3).clamp(3, 12));
    let basename_budget = width - directory_budget - 1 - slash_width;
    let mut out = ellipsized(0, dirname_end, directory_budget, true);
    out.push(('/', Some(dirname_end)));
    out.extend(ellipsized(basename_start, path_len, basename_budget, false));
    with_slash(out)
}

/// Pick the widest hint variant that fits — the reference degrades its nav
/// hints stepwise instead of clipping them.
pub fn degrade_hint(hint: &'static str, width: usize) -> &'static str {
    let variants: &[&'static str] = if hint == HINT_USE {
        &[
            HINT_USE,
            "↑↓ Navigate  Enter Use  Esc Close",
            "↑↓ Move  Enter  Esc",
            "Enter Use  Esc Close",
            "Enter Esc",
        ]
    } else if hint == HINT_SKILLS {
        &[
            HINT_SKILLS,
            "↑↓ Navigate  Tab Source  Enter Use  Esc Close",
            "↑↓ Move  Tab Source  Enter  Esc",
            "Enter Use  Esc Close",
            "Enter Esc",
        ]
    } else if hint == HINT_MODELS {
        &[
            HINT_MODELS,
            "↑↓ Navigate  Tab Provider  Enter Use  Esc Close",
            "↑↓ Move  Tab Provider  Enter  Esc",
            "Enter Use  Esc Close",
            "Enter Esc",
        ]
    } else if hint == HINT_SESSIONS {
        &[
            HINT_SESSIONS,
            "↑↓ Navigate  Tab Scope  Enter Resume  Esc Close",
            "↑↓ Move  Tab Scope  Enter  Esc",
            "Enter Resume  Esc Close",
            "Enter Esc",
        ]
    } else {
        return hint;
    };
    variants
        .iter()
        .copied()
        .find(|v| v.chars().count() <= width)
        .unwrap_or(variants[variants.len() - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_prefers_tight_early_matches() {
        // Subsequence required.
        assert!(fuzzy_score("xyz", "composer.rs").is_none());
        // Tighter match beats scattered.
        let tight = fuzzy_score("comp", "src/tui/composer.rs").unwrap();
        let scattered = fuzzy_score("comp", "core/completions.rs").unwrap();
        assert!(tight < usize::MAX / 2 && scattered < usize::MAX / 2);
        // Case-insensitive.
        assert!(fuzzy_score("LOG", "/login").is_some());
        // Empty query keeps everything, original order.
        assert_eq!(fuzzy_score("", "anything"), Some(usize::MAX / 2));
    }

    /// Shift+tab walks the tabs backward, wrapping at the first tab — the
    /// forward cycle reversed, so Tab/Shift+Tab bracket the picker's tabs.
    #[test]
    fn tabs_cycle_backward_and_wrap() {
        fn row(tab: usize) -> MenuItem {
            MenuItem {
                label: "item".into(),
                description: String::new(),
                meta: String::new(),
                value: "item".into(),
                tab: Some(tab),
            }
        }
        let mut menu = Menu::new(
            MenuKind::Models,
            "Models",
            HINT_MODELS,
            vec![row(0), row(1), row(2)],
        )
        .with_tabs(
            vec!["All".into(), "One".into(), "Two".into()],
            Some(0),
            0,
            "",
        );
        assert_eq!(menu.active_tab, 0);
        menu.cycle_tab_back();
        assert_eq!(
            menu.active_tab, 2,
            "back from the first tab wraps to the last"
        );
        menu.cycle_tab_back();
        assert_eq!(menu.active_tab, 1);
        menu.cycle_tab();
        assert_eq!(menu.active_tab, 2, "forward and backward round-trip");
    }
}
