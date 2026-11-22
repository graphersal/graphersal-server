//! The layout of everything the console prints, the graphersal CLI's layout (its issue #19):
//!
//! - [`Note`]: a result or a refusal as a short headline, then one indented `key: value` per
//!   line (values aligned in one column), every shell command on a line of its own under a
//!   `# comment`; never wrapped, so a path or a command copies in one piece.
//! - [`Section`] + [`render`]: blocks under a `── Title ───` heading (the `serve` start block,
//!   `db list`, `models list`, `db backups`): `key: value` rows whose values start in one
//!   column across all sections, a long value wrapped at [`WIDTH`] under its value column,
//!   [`Section::unwrapped`] never wrapped (commands, paths), [`Section::note`] keeping a note's
//!   layout, [`Section::lines`] for a [`table`] of aligned columns.
//!
//! Copied from the graphersal CLI's `crates/graphersal-cli/src/sections.rs` and the `Note` of
//! its `src/out.rs` (Apache-2.0, same authors; this crate cannot depend on the CLI), extended
//! with tables and aligned note details. Only ASCII and the box-drawing `─` the CLI uses
//! (Windows terminals show both).

/// The width the blocks are laid out for (the banner is as wide).
pub(crate) const WIDTH: usize = 78;

/// One line of a section.
enum Line {
    /// `key: value`, wrapped when the flag is set.
    Row(String, String, bool),
    /// A line printed as it is, indented by two spaces (a table row).
    Raw(String),
    /// The blank line between two notes.
    Blank,
}

/// One section of a block: a heading and its lines.
pub(crate) struct Section {
    title: String,
    lines: Vec<Line>,
    /// Whether the last lines are a note (the next note is set apart by a blank line).
    after_note: bool,
}

impl Section {
    /// An empty section titled `title`.
    pub(crate) fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            lines: Vec::new(),
            after_note: false,
        }
    }

    /// Adds the row `key: value`.
    pub(crate) fn row(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.after_note = false;
        self.lines.push(Line::Row(key.into(), value.into(), true));
    }

    /// Adds the row `key: value`, never wrapped, so a command or a path copies in one piece.
    pub(crate) fn unwrapped(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.after_note = false;
        self.lines.push(Line::Row(key.into(), value.into(), false));
    }

    /// Adds `lines` as they are (a [`table`]), each indented by two spaces; they do not count
    /// for the value column.
    pub(crate) fn lines(&mut self, lines: impl IntoIterator<Item = String>) {
        self.after_note = false;
        self.lines.extend(lines.into_iter().map(Line::Raw));
    }

    /// Adds the note `text` (a headline, then lines indented by two spaces) under `key`: the
    /// headline wraps in the row; each further line follows under the value column, a detail
    /// wrapped, a `# comment` and the command lines after it never wrapped. A note right
    /// after another note is set apart by a blank line.
    pub(crate) fn note(&mut self, key: &str, text: &str) {
        if self.after_note {
            self.lines.push(Line::Blank);
        }
        let mut lines = text.lines();
        let headline = lines.next().unwrap_or_default();
        self.lines
            .push(Line::Row(key.to_string(), headline.to_string(), true));
        let mut commands = false;
        for line in lines {
            let line = line.strip_prefix("  ").unwrap_or(line);
            commands |= line.starts_with("# ");
            self.lines
                .push(Line::Row(String::new(), line.to_string(), !commands));
        }
        self.after_note = true;
    }

    /// Whether the section has no line (it is left out).
    pub(crate) fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// The text of `sections` (empty ones left out), sections separated by a blank line.
pub(crate) fn render(sections: &[Section]) -> String {
    let sections: Vec<&Section> = sections.iter().filter(|s| !s.is_empty()).collect();
    // "  key:" padded to the longest key of the whole block, then one space.
    let key_width = sections
        .iter()
        .flat_map(|s| s.lines.iter())
        .filter_map(|line| match line {
            Line::Row(key, _, _) => Some(key.chars().count() + 1),
            Line::Raw(_) | Line::Blank => None,
        })
        .max()
        .unwrap_or(0);
    let indent = 2 + key_width + 1;
    let mut out = Vec::new();
    for section in sections {
        if !out.is_empty() {
            out.push(String::new());
        }
        out.push(heading(&section.title));
        for line in &section.lines {
            let (key, value, wraps) = match line {
                Line::Row(key, value, wraps) => (key, value, *wraps),
                Line::Raw(text) => {
                    out.push(format!("  {text}").trim_end().to_string());
                    continue;
                }
                Line::Blank => {
                    out.push(String::new());
                    continue;
                }
            };
            // An unwrapped value keeps its lines as they are (the indentation of a command
            // spanning lines included).
            let mut lines = if wraps {
                wrap(value, WIDTH.saturating_sub(indent).max(20))
            } else {
                value.split('\n').map(str::to_string).collect()
            }
            .into_iter();
            let first = lines.next().unwrap_or_default();
            let key = if key.is_empty() {
                String::new()
            } else {
                format!("{key}:")
            };
            out.push(format!("  {key:key_width$} {first}").trim_end().to_string());
            out.extend(lines.map(|line| format!("{:indent$}{line}", "")));
        }
    }
    out.join("\n")
}

/// `── Title ───...` as wide as [`WIDTH`].
pub(crate) fn heading(title: &str) -> String {
    let used = 3 + title.chars().count() + 1;
    format!("── {title} {}", "─".repeat(WIDTH.saturating_sub(used)))
}

/// The lines of a table: `header` and `rows` in columns as wide as their widest cell, two
/// spaces apart, the last column not padded. A cell is one line (only its first line is
/// shown).
pub(crate) fn table(header: &[&str], rows: &[Vec<String>]) -> Vec<String> {
    let header: Vec<String> = header.iter().map(|h| h.to_string()).collect();
    let all: Vec<Vec<&str>> = std::iter::once(&header)
        .chain(rows.iter())
        .map(|row| {
            row.iter()
                .map(|cell| cell.lines().next().unwrap_or_default())
                .collect()
        })
        .collect();
    let columns = all.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; columns];
    for row in &all {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    all.iter()
        .map(|row| {
            let mut line = String::new();
            for (i, cell) in row.iter().enumerate() {
                if i + 1 == row.len() {
                    line.push_str(cell);
                } else {
                    line.push_str(&format!("{cell:<width$}  ", width = widths[i]));
                }
            }
            line.trim_end().to_string()
        })
        .collect()
}

/// The most words an item of a comma-separated list has (`Label 1,000`, `part 1.2 KiB`); a
/// paragraph with a longer item is prose.
const LIST_ITEM_WORDS: usize = 3;

/// The size units [`wrap`] keeps on the line of their number (`1.2 KiB`).
const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

/// The lines of `text` at most `width` characters wide, broken at every `\n` and between the
/// items of a comma-separated list (`person 4, software 2`); an item longer than `width` is
/// broken at spaces, keeping a size unit with its number (a word longer than `width` keeps a
/// line of its own).
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut pieces: Vec<String> = Vec::new();
        let items: Vec<&str> = paragraph.split(", ").collect();
        // A list (`person 4, software 2`: short items) keeps each item on one line; prose
        // wraps word by word, so a line is never cut early at a comma.
        let is_list = items
            .iter()
            .all(|item| item.split_whitespace().count() <= LIST_ITEM_WORDS);
        if !is_list {
            pieces.extend(words(paragraph));
        }
        for (i, item) in items.iter().enumerate().filter(|_| is_list) {
            let item = if i + 1 < items.len() {
                format!("{item},")
            } else {
                (*item).to_string()
            };
            if item.chars().count() <= width {
                pieces.push(item);
            } else {
                pieces.extend(words(&item));
            }
        }
        let mut line = String::new();
        for piece in pieces.iter().filter(|piece| !piece.is_empty()) {
            let needed =
                line.chars().count() + usize::from(!line.is_empty()) + piece.chars().count();
            if !line.is_empty() && needed > width {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(piece);
        }
        lines.push(line);
    }
    lines
}

/// The words of `text`, a size unit joined to the number before it.
fn words(text: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for word in text.split(' ').filter(|word| !word.is_empty()) {
        let unit = word.trim_end_matches([',', ';', ')']);
        match words.last_mut() {
            Some(last) if UNITS.contains(&unit) => {
                last.push(' ');
                last.push_str(word);
            }
            _ => words.push(word.to_string()),
        }
    }
    words
}

/// One line of a [`Note`] after its headline.
enum NoteLine {
    /// `key: value` (a value of several lines continues under the value column).
    Detail(String, String),
    /// A shell command under a `# comment`.
    Command(String, String),
}

/// A result or a refusal in the note layout (module docs).
pub(crate) struct Note {
    headline: String,
    lines: Vec<NoteLine>,
}

impl Note {
    /// A note whose headline is `headline`.
    pub(crate) fn new(headline: impl Into<String>) -> Self {
        Self {
            headline: headline.into(),
            lines: Vec::new(),
        }
    }

    /// Adds the detail line `key: value`.
    pub(crate) fn detail(mut self, key: &str, value: impl Into<String>) -> Self {
        self.lines
            .push(NoteLine::Detail(key.to_string(), value.into()));
        self
    }

    /// Adds the shell command `command` on a line of its own under the comment `comment`.
    pub(crate) fn command(mut self, comment: &str, command: impl Into<String>) -> Self {
        self.lines
            .push(NoteLine::Command(comment.to_string(), command.into()));
        self
    }

    /// The note's text: the headline, then every line indented by two spaces, the detail
    /// values in one column.
    pub(crate) fn text(&self) -> String {
        let key_width = self
            .lines
            .iter()
            .filter_map(|line| match line {
                NoteLine::Detail(key, _) => Some(key.chars().count() + 1),
                NoteLine::Command(..) => None,
            })
            .max()
            .unwrap_or(0);
        let mut out = vec![self.headline.clone()];
        for line in &self.lines {
            match line {
                NoteLine::Detail(key, value) => {
                    let mut values = value.split('\n');
                    let key = format!("{key}:");
                    out.push(
                        format!("  {key:key_width$} {}", values.next().unwrap_or_default())
                            .trim_end()
                            .to_string(),
                    );
                    out.extend(values.map(|v| format!("{:w$}{v}", "", w = 2 + key_width + 1)));
                }
                NoteLine::Command(comment, command) => {
                    out.push(format!("  # {comment}"));
                    out.extend(command.lines().map(|line| format!("  {line}")));
                }
            }
        }
        out.join("\n")
    }
}

/// A byte count for people (`512 B`, `1.5 KiB`), as the CLI shows it.
pub(crate) fn bytes(n: u64) -> String {
    graphersal_session::store::bytes_text(n)
}

/// `n thing` / `n things`.
pub(crate) fn count(n: usize, thing: &str) -> String {
    if n == 1 {
        format!("1 {thing}")
    } else {
        format!("{n} {thing}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_start_in_one_column_and_sections_are_separated() {
        let mut server = Section::new("Server");
        server.row("directory", "srv");
        let mut databases = Section::new("Databases");
        databases.lines(table(
            &["NAME", "STATE"],
            &[vec!["a".into(), "loaded".into()]],
        ));
        databases.row("aliases", "cur -> a");
        let empty = Section::new("Embedding models");
        let text = render(&[server, empty, databases]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0].chars().count(), WIDTH);
        assert!(lines[0].starts_with("── Server ───"));
        assert_eq!(lines[1], "  directory: srv");
        assert_eq!(lines[2], "");
        assert!(lines[3].starts_with("── Databases ───"));
        assert_eq!(lines[4], "  NAME  STATE");
        assert_eq!(lines[5], "  a     loaded");
        assert_eq!(lines[6], "  aliases:   cur -> a");
        assert!(!text.contains("Embedding models"), "{text}");
    }

    #[test]
    fn long_values_wrap_under_their_column_and_unwrapped_ones_do_not() {
        let mut section = Section::new("MCP");
        section.row(
            "token",
            "none: anyone who reaches the port may call the endpoint, which reads and changes \
             every loaded database",
        );
        section.unwrapped("connect", format!("claude mcp add {}", "x".repeat(90)));
        let text = render(&[section]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[1],
            "  token:   none: anyone who reaches the port may call the endpoint, which"
        );
        assert!(
            lines[2].starts_with("           reads and changes"),
            "{text}"
        );
        assert!(lines[3].ends_with(&"x".repeat(90)), "{text}");
        assert!(
            lines
                .iter()
                .filter(|l| !l.starts_with("  connect:"))
                .all(|l| l.chars().count() <= WIDTH),
            "{text}"
        );
    }

    #[test]
    fn a_note_in_a_section_keeps_its_layout() {
        let mut section = Section::new("Problems");
        section.note(
            "broken",
            "failed: the store has no GRAPH file\n  # see why\n  graphersal store check x",
        );
        section.note("other", "needs-model");
        let text = render(&[section]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[1], "  broken: failed: the store has no GRAPH file");
        assert_eq!(lines[2], "          # see why");
        assert_eq!(lines[3], "          graphersal store check x");
        assert_eq!(lines[4], "");
        assert_eq!(lines[5], "  other:  needs-model");
    }

    #[test]
    fn tables_align_their_columns() {
        let lines = table(
            &["NAME", "STATE", "DESCRIPTION"],
            &[
                vec!["acme".into(), "unloaded".into(), "Acme graph".into()],
                vec!["b".into(), "failed\nwhy".into(), String::new()],
            ],
        );
        assert_eq!(
            lines,
            vec![
                "NAME  STATE     DESCRIPTION",
                "acme  unloaded  Acme graph",
                "b     failed",
            ]
        );
    }

    #[test]
    fn a_note_aligns_its_details_and_puts_commands_under_comments() {
        let note = Note::new("Created database 'acme'")
            .detail("kind", "plain")
            .detail("state", "failed\nwhy it failed")
            .command("load it", "graphersal-server db load acme --dir srv");
        assert_eq!(
            note.text(),
            "Created database 'acme'\n  kind:  plain\n  state: failed\n         why it failed\n  \
             # load it\n  graphersal-server db load acme --dir srv"
        );
    }
}
