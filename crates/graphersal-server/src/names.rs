//! Names of databases, aliases and users.
//!
//! Two rules, as for the reserved names:
//!
//! - A NEW name ([`validate_new_name`] for databases and aliases, [`validate_new_user_name`] for
//!   users) is `[a-z0-9]` first, `[a-z0-9._-]` inside, a letter or a digit LAST, no `..`, at
//!   most [`MAX_NAME_LEN`] (32) characters (dots allow `product-2026.3`). It is refused where a
//!   name is made: create, import (upload, path, inbox), clone, restore into another name,
//!   alias set (a new alias) and alias rename (the new name), user add.
//! - An EXISTING name ([`validate_name`]) follows the rule of 0.2.0 and earlier,
//!   `[a-z0-9][a-z0-9._-]{0,62}` not ending in '.' ([`MAX_EXISTING_NAME_LEN`]): a database,
//!   alias, user or grant made before 0.3.0 with a longer name, a trailing '-' or '_' or a `..`
//!   is still loaded, looked up, served under `/db/<name>/`, granted and restored under its own
//!   name.
//!
//! Databases and aliases share ONE namespace, unique case-insensitively: since a valid name
//! holds no upper-case letter, lookups fold the name given to lower case ([`fold`]) and `ACME`
//! finds `acme`.
//!
//! A name is also a directory name under `databases/`, so the names Windows reserves for
//! devices (`con`, `nul`, `com1`, ... also with an extension such as `con.db`) and names ending
//! in a dot (Windows drops it) are refused on every platform: a server directory stays portable.
//! The 32-character limit keeps the paths under a server directory short (a database's Store
//! adds its own directories and file names below `databases/<name>/`), which matters on Windows,
//! where a path longer than 260 characters (`MAX_PATH`) fails unless long paths are enabled.
//!
//! A few names are reserved for NEW databases and aliases ([`validate_new_name`], [`RESERVED`]):
//! `jobs`, because the default backup root of backup jobs is `backups/jobs/`, the directory the
//! admin backups of a database named `jobs` would use. An existing database of a reserved name
//! (made before the reservation) is still served; only creating one is refused.
//!
//! The web UI mirrors the new rule with the same checks in the same order and the same texts
//! (`ui/admin-api.js`, `newNameProblem`); a change here changes it there.

use crate::error::ServerError;

/// The longest NEW name, in bytes (= characters: names are ASCII).
pub const MAX_NAME_LEN: usize = 32;

/// The longest EXISTING name (the rule of 0.2.0 and earlier), in bytes.
pub const MAX_EXISTING_NAME_LEN: usize = 63;

/// The device names Windows reserves (case-insensitive, also before an extension).
const WINDOWS_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com0", "com1", "com2", "com3", "com4", "com5", "com6", "com7",
    "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// The names a new database or alias cannot take ([`validate_new_name`]).
pub const RESERVED: &[&str] = &["jobs"];

/// Whether `name` (folded) is a reserved name ([`RESERVED`]).
pub fn is_reserved(name: &str) -> bool {
    RESERVED.contains(&fold(name).as_str())
}

/// The help of a refused reserved name.
pub const RESERVED_HELP: &str = "pick another name, such as 'jobs-db'";

/// Checks the name of a NEW database or alias (module docs): the new rule
/// ([`validate_new_user_name`]), and not a reserved name ([`RESERVED`]);
/// `Err(`[`ServerError::InvalidName`]`)` whose message names the reason.
pub fn validate_new_name(name: &str) -> Result<(), ServerError> {
    validate_new_user_name(name)?;
    if is_reserved(name) {
        return Err(ServerError::InvalidName {
            name: name.to_string(),
            message: format!(
                "the name is reserved: backup jobs write into backups/jobs/ by default, where \
                 the admin backups of a database named 'jobs' would go; {RESERVED_HELP}"
            ),
        });
    }
    Ok(())
}

/// Checks the name of a NEW user (module docs; the reserved user names are the access file's
/// check): the new rule without the reserved database names. The checks run in this order, each
/// with its own message: empty, longer than [`MAX_NAME_LEN`], the first character, the
/// characters, the last character, `..`, a Windows device name.
pub fn validate_new_user_name(name: &str) -> Result<(), ServerError> {
    let invalid = |message: &str| {
        Err(ServerError::InvalidName {
            name: name.to_string(),
            message: message.to_string(),
        })
    };
    if name.is_empty() {
        return invalid("a name cannot be empty");
    }
    if name.len() > MAX_NAME_LEN {
        return invalid("a name has at most 32 characters");
    }
    check_characters(name)?;
    if !name
        .bytes()
        .last()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return invalid("a name ends with a lower-case letter or a digit");
    }
    if name.contains("..") {
        return invalid("a name cannot hold '..'");
    }
    check_device(name)
}

/// Checks an EXISTING database, alias or user name (the rule of 0.2.0 and earlier, module
/// docs): `Err(`[`ServerError::InvalidName`]`)` says why it is not one.
pub fn validate_name(name: &str) -> Result<(), ServerError> {
    let invalid = |message: &str| {
        Err(ServerError::InvalidName {
            name: name.to_string(),
            message: message.to_string(),
        })
    };
    if name.is_empty() {
        return invalid("a name cannot be empty");
    }
    if name.len() > MAX_EXISTING_NAME_LEN {
        return invalid("a name has at most 63 characters");
    }
    check_characters(name)?;
    if name.ends_with('.') {
        return invalid("a name cannot end with '.'");
    }
    check_device(name)
}

/// The first character and the character set (shared by both rules).
fn check_characters(name: &str) -> Result<(), ServerError> {
    let invalid = |message: &str| {
        Err(ServerError::InvalidName {
            name: name.to_string(),
            message: message.to_string(),
        })
    };
    let first = name.bytes().next().unwrap_or(b'.');
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return invalid("a name starts with a lower-case letter or a digit");
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
    {
        return invalid("a name holds only lower-case letters, digits, '.', '_' and '-'");
    }
    Ok(())
}

/// A Windows device name, also before an extension (shared by both rules).
fn check_device(name: &str) -> Result<(), ServerError> {
    let stem = name.split('.').next().unwrap_or(name);
    if WINDOWS_RESERVED.contains(&stem) {
        return Err(ServerError::InvalidName {
            name: name.to_string(),
            message: "the name is reserved for a device on Windows".to_string(),
        });
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
    fn new_names_follow_the_stricter_rule() {
        for name in [
            "a",
            "0",
            "acme",
            "acme-2026.3",
            "product_v2",
            "a.b-c_d",
            "con-1",
            &"x".repeat(32),
        ] {
            assert!(validate_new_name(name).is_ok(), "{name}");
            assert!(validate_new_user_name(name).is_ok(), "{name}");
        }
        for (name, reason) in [
            ("", "cannot be empty"),
            (&"x".repeat(33) as &str, "at most 32 characters"),
            ("-a", "starts with a lower-case letter or a digit"),
            ("a b", "holds only lower-case letters"),
            ("acme-", "ends with a lower-case letter or a digit"),
            ("acme_", "ends with a lower-case letter or a digit"),
            ("acme.", "ends with a lower-case letter or a digit"),
            ("a..b", "cannot hold '..'"),
            ("con.db", "reserved for a device on Windows"),
        ] {
            let err = validate_new_name(name).unwrap_err();
            assert!(
                matches!(err, ServerError::InvalidName { .. }),
                "{name}: {err}"
            );
            assert!(err.to_string().contains(reason), "{name}: {err}");
            let err = validate_new_user_name(name).unwrap_err();
            assert!(err.to_string().contains(reason), "{name}: {err}");
        }
        // Each of them is still a valid EXISTING name (made before 0.3.0).
        for name in [&"x".repeat(40) as &str, "acme-", "acme_", "a..b"] {
            assert!(validate_name(name).is_ok(), "{name}");
        }
        // A reserved database name is a valid user name.
        assert!(validate_new_user_name("jobs").is_ok());
    }

    #[test]
    fn reserved_names_are_refused_only_for_new_databases_and_aliases() {
        assert!(validate_name("jobs").is_ok());
        let err = validate_new_name("jobs").unwrap_err();
        assert!(matches!(err, ServerError::InvalidName { .. }), "{err}");
        let text = err.to_string();
        assert!(text.contains("reserved"), "{text}");
        assert!(text.contains("backups/jobs/"), "{text}");
        assert!(is_reserved("JOBS"));
        for name in ["jobs-db", "jobs.1", "myjobs", "acme"] {
            assert!(validate_new_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn folding_is_lower_case() {
        assert_eq!(fold("ACME-2026.3"), "acme-2026.3");
    }
}
