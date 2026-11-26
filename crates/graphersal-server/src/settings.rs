//! A database's settings as one editable object (round 3 B7): the runtime-editable fields of its
//! `database.toml` (description, read-only, auto-load and the per-database query limit
//! overrides of `[limits.query]`), changed together in ONE write under the registry's write
//! lock, based on the database's revision ([`crate::revision`]); the admin API's
//! `GET|PUT /api/admin/databases/<name>/settings`, the console's `db info` / `db set` and the
//! web UI's Settings tab use it.
//!
//! When a change applies: the description and auto-load at once (auto-load is read at the next
//! server start); read-only at once (a loaded database's context is flipped, a request already
//! running finishes under the old mode); a query limit override at the database's NEXT LOAD
//! (unload + load, or a server restart): graphersal-serve 0.8.0 fixes a context's session
//! presets when it is built, so a loaded database keeps the limits it was loaded with until
//! then ([`crate::loader::Loader::limits_in_effect`]; the settings answer shows both and
//! `reloadNeeded`).

use serde_json::Value;

use crate::error::ServerError;
use crate::limits::{MAX_LIMIT_VALUE, QUERY_LIMIT_KEYS};

/// The longest description, in characters.
pub(crate) const MAX_DESCRIPTION_CHARS: usize = 1000;

/// When a changed query limit override applies (shown by the UI, the console and the docs).
pub(crate) const LIMITS_APPLY_TEXT: &str = "a query limit change applies at the database's next \
     load: unload and load it (or restart the server); a loaded database keeps the limits it was \
     loaded with until then";

/// A change of a database's settings: every field optional (`None` keeps it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SettingsChange {
    /// The new description (empty clears it).
    pub(crate) description: Option<String>,
    /// Read-only on or off.
    pub(crate) read_only: Option<bool>,
    /// Auto-load on or off.
    pub(crate) auto_load: Option<bool>,
    /// Query limit overrides: `(key, Some(value))` sets one, `(key, None)` clears it (the
    /// server's default applies again). Keys are [`QUERY_LIMIT_KEYS`], each at most once.
    pub(crate) limits: Vec<(String, Option<u64>)>,
}

impl SettingsChange {
    /// Whether nothing would change.
    pub(crate) fn is_empty(&self) -> bool {
        self.description.is_none()
            && self.read_only.is_none()
            && self.auto_load.is_none()
            && self.limits.is_empty()
    }

    /// Checks the change on its own (`name`: the database, for the error): the description's
    /// length and characters, the limit keys. [`ServerError::InvalidInput`] names the problem.
    pub(crate) fn check(&self, name: &str) -> Result<(), ServerError> {
        let invalid = |message: String| ServerError::InvalidInput {
            name: name.to_string(),
            message,
        };
        if self.is_empty() {
            return Err(invalid(
                "nothing to change: send description, readOnly, autoLoad and/or limits".into(),
            ));
        }
        if let Some(description) = &self.description {
            check_description(description).map_err(invalid)?;
        }
        let mut seen = Vec::new();
        for (key, value) in &self.limits {
            check_limit_key(key).map_err(invalid)?;
            if seen.contains(&key) {
                return Err(invalid(format!("limits.{key} is given twice")));
            }
            seen.push(key);
            if let Some(value) = value
                && *value > MAX_LIMIT_VALUE
            {
                return Err(invalid(limit_value_problem(key)));
            }
        }
        Ok(())
    }
}

/// `Err` with the reason when `description` is not a usable description: at most
/// [`MAX_DESCRIPTION_CHARS`] characters, no control characters (it is shown on one line).
pub(crate) fn check_description(description: &str) -> Result<(), String> {
    if description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(format!(
            "a description has at most {MAX_DESCRIPTION_CHARS} characters"
        ));
    }
    if description.chars().any(char::is_control) {
        return Err(
            "a description is one line of text: no line breaks, tabs or control characters".into(),
        );
    }
    Ok(())
}

/// `Err` with the reason when `key` is not a key of `[limits.query]`.
pub(crate) fn check_limit_key(key: &str) -> Result<(), String> {
    if QUERY_LIMIT_KEYS.contains(&key) {
        return Ok(());
    }
    let hint = if key == "memory_limit" || key == "memory_headroom" {
        "; the process memory ceiling is one setting of the whole server: server.toml's [limits]"
    } else {
        ""
    };
    Err(format!(
        "unknown query limit '{key}': the keys are {}{hint}",
        QUERY_LIMIT_KEYS.join(", ")
    ))
}

/// The refusal of a value of the limit `key` that is not a whole number in range.
pub(crate) fn limit_value_problem(key: &str) -> String {
    let zero = if key == "max_value_depth" {
        "0 = the engine's default of 128"
    } else {
        "0 = no limit"
    };
    format!(
        "limits.{key} must be a whole number from 0 to {MAX_LIMIT_VALUE} ({zero}), or null to \
         clear the override"
    )
}

/// The value of the limit `key` in a JSON body: a whole number `0..=MAX_LIMIT_VALUE` (set) or
/// `null` (clear). `Err` with [`limit_value_problem`] for anything else (a negative or
/// fractional number, a string, a boolean), as `server.toml`'s parser refuses them.
pub(crate) fn limit_value_of_json(key: &str, value: &Value) -> Result<Option<u64>, String> {
    match value {
        Value::Null => Ok(None),
        Value::Number(number) => match number.as_u64() {
            Some(n) if n <= MAX_LIMIT_VALUE => Ok(Some(n)),
            _ => Err(limit_value_problem(key)),
        },
        _ => Err(limit_value_problem(key)),
    }
}

/// The value of the limit `key` given as console text: a whole number (`_` allowed as a digit
/// separator, as in TOML). `Err` with [`limit_value_problem`].
pub(crate) fn limit_value_of_text(key: &str, text: &str) -> Result<u64, String> {
    let digits: String = text.trim().chars().filter(|c| *c != '_').collect();
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(limit_value_problem(key));
    }
    match digits.parse::<u64>() {
        Ok(n) if n <= MAX_LIMIT_VALUE => Ok(n),
        _ => Err(limit_value_problem(key)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_change_is_checked_on_its_own() {
        let change = |f: fn(&mut SettingsChange)| {
            let mut change = SettingsChange::default();
            f(&mut change);
            change.check("acme").map_err(|err| err.to_string())
        };
        assert!(change(|_| {}).unwrap_err().contains("nothing to change"));
        change(|c| c.description = Some(String::new())).unwrap();
        change(|c| c.description = Some("Acme's graph, ünïcode ok".into())).unwrap();
        assert!(
            change(|c| c.description = Some("two\nlines".into()))
                .unwrap_err()
                .contains("one line")
        );
        assert!(
            change(|c| c.description = Some("x".repeat(1001)))
                .unwrap_err()
                .contains("at most 1000")
        );
        change(|c| c.description = Some("é".repeat(1000))).unwrap();
        change(|c| {
            c.limits = vec![
                ("timeout_ms".into(), Some(5)),
                ("max_traversers".into(), None),
            ]
        })
        .unwrap();
        assert!(
            change(|c| c.limits = vec![("timeout".into(), Some(5))])
                .unwrap_err()
                .contains("unknown query limit 'timeout': the keys are timeout_ms, max_operations")
        );
        assert!(
            change(|c| c.limits = vec![("memory_limit".into(), Some(5))])
                .unwrap_err()
                .contains("server.toml's [limits]")
        );
        assert!(
            change(|c| c.limits = vec![("timeout_ms".into(), Some(1)), ("timeout_ms".into(), None)])
                .unwrap_err()
                .contains("given twice")
        );
        assert!(
            change(|c| c.limits = vec![("timeout_ms".into(), Some(MAX_LIMIT_VALUE + 1))])
                .unwrap_err()
                .contains("whole number from 0 to 9223372036854775807")
        );
    }

    #[test]
    fn limit_values_are_whole_numbers_in_range_or_null() {
        assert_eq!(limit_value_of_json("timeout_ms", &json!(0)), Ok(Some(0)));
        assert_eq!(
            limit_value_of_json("timeout_ms", &json!(5000)),
            Ok(Some(5000))
        );
        assert_eq!(limit_value_of_json("timeout_ms", &json!(null)), Ok(None));
        for bad in [
            json!(-1),
            json!(1.5),
            json!("5"),
            json!(true),
            json!(u64::MAX),
        ] {
            let err = limit_value_of_json("timeout_ms", &bad).unwrap_err();
            assert!(err.contains("0 = no limit"), "{bad}: {err}");
        }
        assert!(
            limit_value_of_json("max_value_depth", &json!(-1))
                .unwrap_err()
                .contains("default of 128")
        );
        assert_eq!(limit_value_of_text("timeout_ms", "30_000"), Ok(30_000));
        assert_eq!(limit_value_of_text("timeout_ms", " 7 "), Ok(7));
        for bad in ["", "-1", "1.5", "lots", "9223372036854775808", "_"] {
            assert!(limit_value_of_text("timeout_ms", bad).is_err(), "{bad}");
        }
    }
}
