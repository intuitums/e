//! Line diff for the detail viewer, in the reference row grammar:
//!
//! ```text
//!    41   fn before(&self) {
//!    42 -     let old = 1;
//!    42 +     let new = 2;
//!    43   }
//!       ⋯
//! ```
//!
//! `{lineno:>5} {op} {text}` — removed rows carry old-file numbers, added
//! and context rows new-file numbers; unchanged stretches beyond three
//! context lines fold into a `⋯` row. The rows are plain text; the viewer
//! colors the number-and-sign column at paint time.

/// Context lines kept around each change.
const CONTEXT: usize = 3;

/// LCS cell budget. Past it the diff still renders — the common prefix and
/// suffix are exact, and the middle is marked as one replacement.
const MAX_CELLS: usize = 4_000_000;

enum Op {
    /// Context, carrying its new-file line number (1-based) — the number an
    /// editor opening the file today would show.
    Keep(usize),
    Del(usize),
    Add(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ending {
    Lf,
    CrLf,
    None,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct DiffLine<'a> {
    text: &'a str,
    ending: Ending,
}

fn split_lines(text: &str) -> Vec<DiffLine<'_>> {
    text.split_inclusive('\n')
        .map(|line| {
            if let Some(text) = line.strip_suffix("\r\n") {
                DiffLine {
                    text,
                    ending: Ending::CrLf,
                }
            } else if let Some(text) = line.strip_suffix('\n') {
                DiffLine {
                    text,
                    ending: Ending::Lf,
                }
            } else {
                DiffLine {
                    text: line,
                    ending: Ending::None,
                }
            }
        })
        .collect()
}

fn shown_line(line: DiffLine<'_>) -> String {
    match line.ending {
        Ending::CrLf => format!("{}␍", line.text),
        _ => line.text.to_string(),
    }
}

/// Render the diff between two texts. Empty when nothing changed.
pub fn render(before: &str, after: &str) -> String {
    let old = split_lines(before);
    let new = split_lines(after);
    let ops = script(&old, &new);
    if !ops.iter().any(|op| !matches!(op, Op::Keep(..))) {
        return String::new();
    }

    // Which op rows survive: every change, plus CONTEXT of Keep around it.
    let mut keep = vec![false; ops.len()];
    for (i, op) in ops.iter().enumerate() {
        if !matches!(op, Op::Keep(..)) {
            let from = i.saturating_sub(CONTEXT);
            let to = (i + CONTEXT + 1).min(ops.len());
            for slot in keep.iter_mut().take(to).skip(from) {
                *slot = true;
            }
        }
    }

    let mut out = String::new();
    let mut elided = false;
    for (i, op) in ops.iter().enumerate() {
        if !keep[i] {
            if !elided {
                out.push_str("      ⋯\n");
                elided = true;
            }
            continue;
        }
        elided = false;
        let (row, ending) = match op {
            Op::Keep(n) => (
                format!("{:>5}   {}", n, shown_line(new[*n - 1])),
                new[*n - 1].ending,
            ),
            Op::Del(o) => (
                format!("{:>5} - {}", o, shown_line(old[*o - 1])),
                old[*o - 1].ending,
            ),
            Op::Add(n) => (
                format!("{:>5} + {}", n, shown_line(new[*n - 1])),
                new[*n - 1].ending,
            ),
        };
        out.push_str(&row);
        out.push('\n');
        if !matches!(op, Op::Keep(_)) && ending == Ending::None {
            out.push_str("      \\ No newline at end of file\n");
        }
    }
    out.pop();
    out
}

/// The edit script: trim the exact common prefix and suffix, then LCS over
/// the middle (or one replacement block when the middle is too large).
fn script(old: &[DiffLine<'_>], new: &[DiffLine<'_>]) -> Vec<Op> {
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old.len() - prefix
        && suffix < new.len() - prefix
        && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let mid_old = &old[prefix..old.len() - suffix];
    let mid_new = &new[prefix..new.len() - suffix];

    let mut ops = Vec::with_capacity(old.len().max(new.len()));
    for i in 0..prefix {
        ops.push(Op::Keep(i + 1));
    }
    if mid_old.len().saturating_mul(mid_new.len()) <= MAX_CELLS {
        lcs_ops(mid_old, mid_new, prefix, &mut ops);
    } else {
        for (i, _) in mid_old.iter().enumerate() {
            ops.push(Op::Del(prefix + i + 1));
        }
        for (i, _) in mid_new.iter().enumerate() {
            ops.push(Op::Add(prefix + i + 1));
        }
    }
    for i in 0..suffix {
        ops.push(Op::Keep(new.len() - suffix + i + 1));
    }
    ops
}

/// Standard LCS table walk over the trimmed middle; deletions before
/// insertions inside a replaced run, the conventional order.
fn lcs_ops(old: &[DiffLine<'_>], new: &[DiffLine<'_>], offset: usize, ops: &mut Vec<Op>) {
    let (rows, cols) = (old.len(), new.len());
    let mut table = vec![0u32; (rows + 1) * (cols + 1)];
    for o in (0..rows).rev() {
        for n in (0..cols).rev() {
            table[o * (cols + 1) + n] = if old[o] == new[n] {
                table[(o + 1) * (cols + 1) + n + 1] + 1
            } else {
                table[(o + 1) * (cols + 1) + n].max(table[o * (cols + 1) + n + 1])
            };
        }
    }
    let (mut o, mut n) = (0, 0);
    while o < rows && n < cols {
        if old[o] == new[n] {
            ops.push(Op::Keep(offset + n + 1));
            o += 1;
            n += 1;
        } else if table[(o + 1) * (cols + 1) + n] >= table[o * (cols + 1) + n + 1] {
            ops.push(Op::Del(offset + o + 1));
            o += 1;
        } else {
            ops.push(Op::Add(offset + n + 1));
            n += 1;
        }
    }
    while o < rows {
        ops.push(Op::Del(offset + o + 1));
        o += 1;
    }
    while n < cols {
        ops.push(Op::Add(offset + n + 1));
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn a_one_line_change_shows_numbers_context_and_markers() {
        let before = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let after = "a\nb\nc\nd\nE\nf\ng\nh\n";
        let out = render(before, after);
        // Old number on the removal, new number on the addition, three
        // context lines each side, the far edges elided.
        assert!(out.contains("    5 - e"), "{out}");
        assert!(out.contains("    5 + E"), "{out}");
        assert!(out.contains("    4   d"), "{out}");
        assert!(out.contains("    8   h"), "{out}");
        assert!(out.contains("      ⋯"), "{out}");
        assert!(!out.contains("  1   a"), "{out}");
    }

    #[test]
    fn identical_texts_render_nothing() {
        assert_eq!(render("same\n", "same\n"), "");
    }

    #[test]
    fn crlf_to_lf_is_visible() {
        let out = render("one\r\ntwo\r\n", "one\ntwo\n");
        assert!(out.contains("    1 - one␍"), "{out}");
        assert!(out.contains("    1 + one"), "{out}");
        assert!(!out.contains("    1 + one␍"), "{out}");
    }

    #[test]
    fn final_newline_changes_are_visible() {
        let removed = render("one\n", "one");
        assert!(removed.contains("    1 - one"), "{removed}");
        assert!(removed.contains("    1 + one"), "{removed}");
        assert!(
            removed.contains("\\ No newline at end of file"),
            "{removed}"
        );

        let added = render("one", "one\n");
        assert!(added.contains("    1 - one"), "{added}");
        assert!(added.contains("    1 + one"), "{added}");
        assert!(added.contains("\\ No newline at end of file"), "{added}");
    }

    #[test]
    fn a_new_file_is_all_additions() {
        let out = render("", "one\ntwo\n");
        assert!(out.contains("    1 + one"), "{out}");
        assert!(out.contains("    2 + two"), "{out}");
        assert!(!out.contains('-'), "{out}");
    }

    #[test]
    fn insertion_shifts_following_numbers() {
        let before = "a\nb\nc\n";
        let after = "a\nnew\nb\nc\n";
        let out = render(before, after);
        assert!(out.contains("    2 + new"), "{out}");
        // Context after the insertion carries new-file numbers.
        assert!(out.contains("    3   b"), "{out}");
        assert!(out.contains("    4   c"), "{out}");
    }

    #[test]
    fn oversized_middles_still_render_a_replacement() {
        // Force the fallback path with unique lines beyond the cell budget.
        let before: String = (0..2_100).map(|i| format!("x{i}\n")).collect();
        let after: String = (0..2_100).map(|i| format!("y{i}\n")).collect();
        let out = render(&before, &after);
        assert!(out.contains("    1 - x0"), "{}", &out[..200]);
        assert!(out.contains("    1 + y0"), "{}", &out[..200]);
    }
}
