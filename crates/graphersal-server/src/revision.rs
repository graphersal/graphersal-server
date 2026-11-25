//! Optimistic concurrency for the admin's edits: a `revision` per shared object, so a change
//! made from a stale page is refused instead of silently undoing someone else's change.
//!
//! A revision is the first 16 hex digits of the SHA-256 of the object's text as the server
//! writes it to its file ([`of`]): the same state always has the same revision (also after a
//! restart, also when the console changed it), and any change gives a new one. Objects:
//!
//! | Object | File | Revision in | Checked by |
//! |---|---|---|---|
//! | users and grants | `access.toml` | `GET users`, `GET grants` (`revision`, `ETag`) | every `users...` and `grants...` change |
//! | aliases | `aliases.toml` | `GET aliases` (`revision`, `ETag`), `GET databases` (`aliasesRevision`) | `PUT`/`DELETE aliases/<a>`, `POST aliases/<a>/rename` |
//! | a database's settings | `databases/<name>/database.toml` | every entry (`revision`; `GET databases/<name>` also `ETag`) | `PUT databases/<name>/read-only`, `PUT databases/<name>/auto-load` |
//!
//! `server.toml` has no setting an admin request changes yet; a later one takes its revision
//! the same way (its text through [`of`], checked under the lock its writer holds).
//!
//! A change MAY name the revision it was based on: the JSON body's `revision`, else the
//! `If-Match` header (`"<rev>"`, `W/"<rev>"` or the bare value; `*` = no check). The check
//! runs under the same lock as the change (`crate::registry`, `crate::access`), right before
//! it: another revision refuses it with [`ServerError::Conflict`] (`409 {"error", "help":
//! "changed by someone else: reload", "revision": <current>}`), nothing changed. A change
//! WITHOUT a revision is applied as before (the console, scripts and older clients: they act
//! on the current state).

use crate::error::ServerError;

/// The help of a refused change ([`ServerError::Conflict`]).
pub const CONFLICT_HELP: &str = "changed by someone else: reload";

/// The revision of an object whose file text is `text` (module docs).
pub fn of(text: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, text);
    digest.as_ref()[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Refuses a change based on `expected` when the object (named `what` in the error) is at
/// `current` now; `None` expects nothing (the change is applied).
pub fn check(expected: Option<&str>, current: &str, what: &str) -> Result<(), ServerError> {
    match expected {
        Some(expected) if expected != current => Err(ServerError::Conflict {
            what: what.to_string(),
            revision: current.to_string(),
        }),
        _ => Ok(()),
    }
}

/// The revision an `If-Match` header value names: the entity tag without `W/` and quotes;
/// `None` for `*` or an empty value (no check).
pub fn from_if_match(value: &str) -> Option<String> {
    let value = value.trim();
    let value = value.strip_prefix("W/").unwrap_or(value);
    let value = value.trim_matches('"').trim();
    (!value.is_empty() && value != "*").then(|| value.to_string())
}

/// The `ETag` header value of `revision` (a strong entity tag).
pub fn etag(revision: &str) -> String {
    format!("\"{revision}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revisions_follow_the_text() {
        let a = of(b"x = 1\n");
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(a, of(b"x = 1\n"));
        assert_ne!(a, of(b"x = 2\n"));
    }

    #[test]
    fn a_stale_revision_is_a_conflict_and_none_is_accepted() {
        assert!(check(None, "abc", "aliases").is_ok());
        assert!(check(Some("abc"), "abc", "aliases").is_ok());
        let err = check(Some("old"), "new", "aliases").unwrap_err();
        assert!(
            matches!(&err, ServerError::Conflict { what, revision } if what == "aliases" && revision == "new"),
            "{err:?}"
        );
        assert!(err.to_string().contains("changed by someone else"), "{err}");
    }

    #[test]
    fn if_match_values_are_read_as_entity_tags() {
        assert_eq!(from_if_match("\"abc\""), Some("abc".to_string()));
        assert_eq!(from_if_match("W/\"abc\""), Some("abc".to_string()));
        assert_eq!(from_if_match(" abc "), Some("abc".to_string()));
        assert_eq!(from_if_match("*"), None);
        assert_eq!(from_if_match(""), None);
        assert_eq!(etag("abc"), "\"abc\"");
    }
}
