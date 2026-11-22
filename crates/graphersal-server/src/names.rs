//! Names of databases and aliases.
//!
//! A name is `[a-z0-9][a-z0-9._-]{0,62}` (dots allow `product-2026.3`). Databases and aliases
//! share ONE namespace, unique case-insensitively: since a valid name holds no upper-case
//! letter, lookups fold the name given to lower case ([`fold`]) and `ACME` finds `acme`.
//!
//! A name is also a directory name under `databases/`, so the names Windows reserves for
//! devices (`con`, `nul`, `com1`, ... also with an extension such as `con.db`) and names ending
//! in a dot (Windows drops it) are refused on every platform: a server directory stays portable.

use crate::error::ServerError;

/// The longest name, in bytes (= characters: names are ASCII).
pub const MAX_NAME_LEN: usize = 63;

/// The device names Windows reserves (case-insensitive, also before an extension).
const WINDOWS_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com0", "com1", "com2", "com3", "com4", "com5", "com6", "com7",
    "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Checks a database or alias name: `Err(`[`ServerError::InvalidName`]`)` says why it is not
/// one.
pub fn validate_name(name: &str) -> Result<(), ServerError> {
    let invalid = |message: &str| {
        Err(ServerError::InvalidName {
            name: name.to_string(),
            message: message.to_string(),
        })
    };
    let Some(first) = name.bytes().next() else {
        return invalid("a name cannot be empty");
    };
    if name.len() > MAX_NAME_LEN {
        return invalid("a name has at most 63 characters");
    }
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return invalid("a name starts with a lower-case letter or a digit");
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
    {
        return invalid("a name holds only lower-case letters, digits, '.', '_' and '-'");
    }
    if name.ends_with('.') {
        return invalid("a name cannot end with '.'");
    }
    let stem = name.split('.').next().unwrap_or(name);
    if WINDOWS_RESERVED.contains(&stem) {
        return invalid("the name is reserved for a device on Windows");
    }
    Ok(())
}

/// The lookup key of a name given by a user: folded to lower case (names are unique
/// case-insensitively).
pub fn fold(name: &str) -> String {
    name.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        for name in [
            "a",
            "0",
            "acme",
            "acme-2026.3",
            "product_v2",
            "a.b-c_d",
            "console",
            "con-1",
            &"x".repeat(63),
        ] {
            assert!(validate_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn invalid_names() {
        for name in [
            "",
            "Acme",
            "ACME",
            ".hidden",
            "-a",
            "_a",
            "a b",
            "a/b",
            "a\\b",
            "..",
            "a.",
            "café",
            "con",
            "nul.db",
            "lpt1",
            &"x".repeat(64),
        ] {
            let err = validate_name(name).unwrap_err();
            assert!(
                matches!(err, ServerError::InvalidName { .. }),
                "{name}: {err}"
            );
            assert!(err.to_string().contains("invalid name"), "{err}");
        }
    }

    #[test]
    fn folding_is_lower_case() {
        assert_eq!(fold("ACME-2026.3"), "acme-2026.3");
    }
}
