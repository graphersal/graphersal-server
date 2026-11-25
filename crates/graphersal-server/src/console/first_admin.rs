//! The one-time print of the first admin's token (`init`, the first `serve` of a directory
//! without `access.toml`): a framed block on stderr that stands out, bold yellow on a
//! terminal, a plain ASCII frame otherwise. Nothing else ever shows the token.

use std::path::Path;

use crate::access::FirstAdmin;

/// The frame's least inner width (the block is 78 columns wide unless a line is longer).
const INNER: usize = 74;

/// The framed block showing `first`'s token; `styled`: for a terminal (box-drawing frame,
/// bold yellow, the token bold), else plain ASCII without escape codes.
pub fn first_admin_text(first: &FirstAdmin, dir: &Path, styled: bool) -> String {
    let lines = [
        "FIRST ADMIN TOKEN - shown once, never again".to_string(),
        String::new(),
        format!(
            "user:   {} (role admin: every database, the admin API)",
            first.name
        ),
        format!("token:  {}", first.token.expose()),
        String::new(),
        "Copy it now: only its SHA-256 hash is kept, in".to_string(),
        format!("  {}", first.path.display()),
        "Send it as \"Authorization: Bearer <token>\", or paste it on the login page.".to_string(),
        "Lost it? This prints a new one (the old one stops working):".to_string(),
        format!(
            "  graphersal-server user token {} --dir {}",
            first.name,
            super::render::shell_word(dir)
        ),
        format!("Server directory: {}", dir.display()),
    ];
    let width = lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
        .max(INNER);
    let (top, side, bottom) = if styled {
        (
            format!("╔═{}═╗", "═".repeat(width)),
            "║",
            format!("╚═{}═╝", "═".repeat(width)),
        )
    } else {
        (
            format!("+-{}-+", "-".repeat(width)),
            "|",
            format!("+-{}-+", "-".repeat(width)),
        )
    };
    let (on, off, strong) = if styled {
        ("\u{1b}[1;33m", "\u{1b}[0m", "\u{1b}[1;97m")
    } else {
        ("", "", "")
    };
    let mut out = vec![format!("{on}{top}{off}")];
    for line in &lines {
        let pad = " ".repeat(width - line.chars().count());
        let body = match line.strip_prefix("token:  ") {
            Some(token) if styled => format!("token:  {strong}{token}{off}{on}"),
            _ => line.clone(),
        };
        out.push(format!("{on}{side} {body}{pad} {side}{off}"));
    }
    out.push(format!("{on}{bottom}{off}"));
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{AccessFile, FIRST_ADMIN};
    use crate::layout::ServerDir;
    use crate::test_support::TempDir;

    fn first() -> (TempDir, FirstAdmin) {
        let temp = TempDir::new("first-admin-text");
        let report = ServerDir::init(temp.path()).unwrap();
        let first = report.first_admin.unwrap();
        (temp, first)
    }

    #[test]
    fn the_plain_block_is_an_ascii_frame_with_the_token() {
        let (temp, first) = first();
        let text = first_admin_text(&first, temp.path(), false);
        assert!(text.is_ascii(), "{text}");
        assert!(!text.contains('\u{1b}'), "{text}");
        let lines: Vec<&str> = text.lines().collect();
        let width = lines[0].chars().count();
        assert!(width >= 78, "{text}");
        assert!(
            lines[0].starts_with("+--") && lines[0].ends_with("-+"),
            "{text}"
        );
        assert_eq!(lines.last(), lines.first());
        for line in &lines[1..lines.len() - 1] {
            assert!(line.starts_with("| ") && line.ends_with(" |"), "{line}");
            assert_eq!(line.chars().count(), width, "{line}");
        }
        assert!(
            lines
                .iter()
                .any(|l| l.contains(&format!("token:  {}", first.token.expose()))),
            "{text}"
        );
        assert!(text.contains("shown once"), "{text}");
        assert!(text.contains(&format!("user:   {FIRST_ADMIN}")), "{text}");
        assert!(text.contains("access.toml"), "{text}");
        // The token works: its hash is the file's.
        let file = AccessFile::read(&ServerDir::new(temp.path()))
            .unwrap()
            .unwrap();
        assert!(file.user_by_token(&first.token.hash()).is_some());
    }

    #[test]
    fn the_terminal_block_is_bold_and_framed() {
        let (temp, first) = first();
        let text = first_admin_text(&first, temp.path(), true);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("\u{1b}[1;33m╔"), "{text}");
        assert!(lines.last().unwrap().contains('╝'), "{text}");
        assert!(lines.iter().all(|l| l.ends_with("\u{1b}[0m")), "{text}");
        assert!(
            text.contains(&format!("\u{1b}[1;97m{}\u{1b}[0m", first.token.expose())),
            "{text}"
        );
        // Without the escape codes every line is as wide as the frame.
        let plain: Vec<String> = lines
            .iter()
            .map(|l| {
                l.replace("\u{1b}[1;33m", "")
                    .replace("\u{1b}[1;97m", "")
                    .replace("\u{1b}[0m", "")
            })
            .collect();
        let width = plain[0].chars().count();
        assert!(
            plain.iter().all(|l| l.chars().count() == width),
            "{plain:?}"
        );
    }
}
