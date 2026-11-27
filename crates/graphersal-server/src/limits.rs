//! Query limits (`[limits.query]`) and the process memory ceiling (`[limits] memory_limit`,
//! `memory_headroom`).
//!
//! `[limits.query]` bounds every query of a database: set in `server.toml` for every database
//! and in a database's `database.toml` to override single keys for that database. Both files
//! are read again at every load: the merged limits ([`QueryLimits::over`]) become the
//! graphersal-session presets of the loaded database ([`QueryLimits::presets`]). A change made
//! through the server (a database's settings, `server.toml` through the admin API) applies at
//! once to the new runs of a loaded database (`Loader::apply_limits`); a hand edit of a file at
//! the next load or the next change through the server. A database's overrides are set or
//! cleared at runtime through its settings (`crate::settings`: the admin API, `db set NAME
//! limit KEY VALUE|--clear`, the web UI's Database settings tab), with the same refusals as
//! this parser. Default: no limit at all (the presets of the graphersal playground).
//!
//! ```toml
//! [limits.query]
//! timeout_ms = 30000              # evaluationTimeout of every traversal and the budget of a run
//! max_operations = 10000000       # Rhai operations one script may run
//! max_string_size = 16777216      # longest string a script or a step may build, in bytes
//! max_array_size = 100000         # largest array a script may build, in items
//! max_map_size = 100000           # largest map a script may build, in entries
//! max_traversers = 10000000       # traversers one step may leave alive
//! max_materialized_bytes = 524288000   # memory one query holds at once
//! max_value_depth = 128           # deepest nesting of arrays and maps in one value
//! ```
//!
//! The key names are the graphersal command line's flags (`--timeout <MS>` -> `timeout_ms`,
//! `--max-operations` -> `max_operations`, ...) with the same meaning: `0` is "no limit" for
//! `timeout_ms` and the `max_*` keys except `max_value_depth`, where `0` is the engine's default
//! (128; a value depth is never unlimited). Setting `0` in `database.toml` lifts a global limit
//! for that database. A set limit is a ceiling (`SessionPresets::with_ceilings`): a query may
//! lower it for itself with `g.with("evaluationTimeout", ..)`, never raise or remove it
//! (`OptionAboveCeiling`); a key left unset is no ceiling.
//!
//! The process memory ceiling is one setting of the whole process (every database), so it is a
//! key of `server.toml`'s `[limits]` only, set once at start
//! ([`graphersal::exec::set_memory_limit`]); `database.toml` refuses it.

use graphersal::exec::{MemoryLimit, set_memory_headroom, set_memory_limit};
use graphersal::prelude::ElementProperty;
use graphersal::script::ScriptLimits;
use graphersal_session::SessionPresets;
use serde::{Deserialize, Deserializer, Serialize};

/// The keys of `[limits.query]` in file order (the `with_*`-less way to name one: the admin
/// API's settings route, the console's `db set NAME limit KEY VALUE`).
pub const QUERY_LIMIT_KEYS: [&str; 8] = [
    "timeout_ms",
    "max_operations",
    "max_string_size",
    "max_array_size",
    "max_map_size",
    "max_traversers",
    "max_materialized_bytes",
    "max_value_depth",
];

/// The largest value a query limit may have through the admin API and the console: the
/// engine's options are signed 64-bit numbers (a larger value would be cut to this one
/// anyway), and a TOML integer is one too.
pub const MAX_LIMIT_VALUE: u64 = i64::MAX as u64;

/// The query limits of `[limits.query]` (module docs): each key optional, a missing key is no
/// limit (or the global value, for a database's override).
///
/// `#[non_exhaustive]`: further limits may be added; build one with [`QueryLimits::new`] and
/// the `with_*` setters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct QueryLimits {
    /// The default `evaluationTimeout` of every traversal and the budget of a whole run (the
    /// Rhai code between the traversals), in milliseconds; `0`: no timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Rhai operations one script may run (a `loop {}` stops here); `0`: unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_operations: Option<u64>,
    /// The longest string a script or a step may build, in bytes (the script limit and the
    /// `traversal.max_string_bytes` default); `0`: unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_string_size: Option<u64>,
    /// The largest array a script may build, in items; `0`: unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_array_size: Option<u64>,
    /// The largest map a script may build, in entries; `0`: unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_map_size: Option<u64>,
    /// The `traversal.max_traversers` default: traversers one step may leave alive; `0`:
    /// unlimited. Missing: the engine default (10M).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_traversers: Option<u64>,
    /// The `traversal.max_materialized_bytes` default: the memory one query holds at once, in
    /// bytes; `0`: unlimited. Missing: the engine default (1 GiB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_materialized_bytes: Option<u64>,
    /// The deepest nesting of arrays and maps in one value (script values and engine-built
    /// values); `0`: the engine default (128). Never unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_value_depth: Option<u64>,
}

impl QueryLimits {
    /// No limit (every key missing).
    pub fn new() -> Self {
        Self::default()
    }

    /// The limits with `timeout_ms`.
    pub fn with_timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = Some(ms);
        self
    }

    /// The limits with `max_operations`.
    pub fn with_max_operations(mut self, n: u64) -> Self {
        self.max_operations = Some(n);
        self
    }

    /// The limits with `max_traversers`.
    pub fn with_max_traversers(mut self, n: u64) -> Self {
        self.max_traversers = Some(n);
        self
    }

    /// Whether no key is set.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The value of the key `key` ([`QUERY_LIMIT_KEYS`]): `None` for an unknown key,
    /// `Some(None)` for a key not set.
    pub fn get(&self, key: &str) -> Option<Option<u64>> {
        Some(match key {
            "timeout_ms" => self.timeout_ms,
            "max_operations" => self.max_operations,
            "max_string_size" => self.max_string_size,
            "max_array_size" => self.max_array_size,
            "max_map_size" => self.max_map_size,
            "max_traversers" => self.max_traversers,
            "max_materialized_bytes" => self.max_materialized_bytes,
            "max_value_depth" => self.max_value_depth,
            _ => return None,
        })
    }

    /// Sets (`Some`) or clears (`None`) the key `key` ([`QUERY_LIMIT_KEYS`]); `false` (nothing
    /// changed) for an unknown key.
    pub fn set(&mut self, key: &str, value: Option<u64>) -> bool {
        match self.slot(key) {
            Some(slot) => {
                *slot = value;
                true
            }
            None => false,
        }
    }

    /// The field of the key `key`.
    fn slot(&mut self, key: &str) -> Option<&mut Option<u64>> {
        Some(match key {
            "timeout_ms" => &mut self.timeout_ms,
            "max_operations" => &mut self.max_operations,
            "max_string_size" => &mut self.max_string_size,
            "max_array_size" => &mut self.max_array_size,
            "max_map_size" => &mut self.max_map_size,
            "max_traversers" => &mut self.max_traversers,
            "max_materialized_bytes" => &mut self.max_materialized_bytes,
            "max_value_depth" => &mut self.max_value_depth,
            _ => return None,
        })
    }

    /// These limits (a database's `[limits.query]`) over `global` (`server.toml`'s): a key set
    /// here wins, a missing one takes the global value.
    pub fn over(&self, global: &QueryLimits) -> QueryLimits {
        QueryLimits {
            timeout_ms: self.timeout_ms.or(global.timeout_ms),
            max_operations: self.max_operations.or(global.max_operations),
            max_string_size: self.max_string_size.or(global.max_string_size),
            max_array_size: self.max_array_size.or(global.max_array_size),
            max_map_size: self.max_map_size.or(global.max_map_size),
            max_traversers: self.max_traversers.or(global.max_traversers),
            max_materialized_bytes: self
                .max_materialized_bytes
                .or(global.max_materialized_bytes),
            max_value_depth: self.max_value_depth.or(global.max_value_depth),
        }
    }

    /// The graphersal-session presets of these limits (module docs): only what is set is
    /// preset, so no key at all gives [`SessionPresets::new`] (no limit).
    pub fn presets(&self) -> SessionPresets {
        if self.is_empty() {
            return SessionPresets::new();
        }
        let mut limits = ScriptLimits::unlimited();
        if let Some(n) = self.max_operations {
            limits.max_operations = n;
        }
        if let Some(n) = self.max_string_size {
            limits.max_string_size = size(n);
        }
        if let Some(n) = self.max_array_size {
            limits.max_array_size = size(n);
        }
        if let Some(n) = self.max_map_size {
            limits.max_map_size = size(n);
        }
        if let Some(n) = self.max_value_depth {
            // `0` is read as the default by the engine.
            limits.max_value_depth = size(n);
        }
        let mut presets = SessionPresets::new()
            .with_limits(limits)
            .with_script_timeout_ms(self.timeout_ms);
        if let Some(ms) = self.timeout_ms.filter(|ms| *ms > 0) {
            presets = presets.with_option("evaluationTimeout", option_value(ms));
        }
        for (key, value) in [
            ("traversal.max_traversers", self.max_traversers),
            (
                "traversal.max_materialized_bytes",
                self.max_materialized_bytes,
            ),
            ("traversal.max_string_bytes", self.max_string_size),
        ] {
            if let Some(n) = value {
                presets = presets.with_option(key, unlimited_at_zero(n));
            }
        }
        if let Some(n) = self.max_value_depth.filter(|n| *n > 0) {
            presets = presets.with_option("traversal.max_value_depth", option_value(n));
        }
        // Ceilings: a query may lower a configured limit for itself (`g.with(..)`), never raise
        // or remove it (`OptionAboveCeiling`). A key left unset is no ceiling.
        presets.with_ceilings(true)
    }

    /// The limits as `key=value` pairs in file order (the start block's row); empty when no
    /// key is set.
    pub fn summary(&self) -> String {
        [
            ("timeout_ms", self.timeout_ms),
            ("max_operations", self.max_operations),
            ("max_string_size", self.max_string_size),
            ("max_array_size", self.max_array_size),
            ("max_map_size", self.max_map_size),
            ("max_traversers", self.max_traversers),
            ("max_materialized_bytes", self.max_materialized_bytes),
            ("max_value_depth", self.max_value_depth),
        ]
        .iter()
        .filter_map(|(key, value)| value.map(|value| format!("{key}={value}")))
        .collect::<Vec<_>>()
        .join(", ")
    }
}

/// The `[limits]` table of a `database.toml`: only `query` (the memory ceiling is server-wide).
///
/// `#[non_exhaustive]`: further per-database limits may be added.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct DatabaseLimits {
    /// The database's query limits, each key over the global one of `server.toml`.
    #[serde(default, skip_serializing_if = "QueryLimits::is_empty")]
    pub query: QueryLimits,
}

impl DatabaseLimits {
    /// The limits with the query limits `query`.
    pub fn with_query(mut self, query: QueryLimits) -> Self {
        self.query = query;
        self
    }

    /// Whether nothing is set.
    pub fn is_empty(&self) -> bool {
        self.query.is_empty()
    }
}

/// Sets the process memory ceiling from `server.toml`'s `[limits]` (`limit`, `headroom`):
/// only what is set; nothing set keeps the library's default (`auto`). `Err` carries the
/// reason.
pub(crate) fn apply_memory_ceiling(
    limit: Option<MemoryLimit>,
    headroom: Option<u64>,
) -> Result<(), String> {
    if let Some(limit) = limit {
        set_memory_limit(limit).map_err(|err| format!("limits.memory_limit: {err}"))?;
    }
    if headroom.is_some() {
        set_memory_headroom(headroom).map_err(|err| format!("limits.memory_headroom: {err}"))?;
    }
    Ok(())
}

/// Sets the process memory ceiling of a running server to `server.toml`'s `[limits]` after a
/// settings change (round 4 B-SETTINGS; the engine's setters apply from now on: an execution
/// that already runs keeps its ceiling): `limit` (`None`: the library's default `auto`) and
/// `headroom` (`None`: the default). `Err` carries the reason.
pub(crate) fn apply_memory_live(
    limit: Option<MemoryLimit>,
    headroom: Option<u64>,
) -> Result<(), String> {
    set_memory_limit(limit.unwrap_or(MemoryLimit::Auto))
        .map_err(|err| format!("limits.memory_limit: {err}"))?;
    set_memory_headroom(headroom).map_err(|err| format!("limits.memory_headroom: {err}"))
}

/// Deserializes `[limits] memory_limit`: a number of bytes (`0` is `"none"`, as the command
/// line's `--memory-limit 0`), `"auto"` or `"none"`.
pub(crate) fn memory_limit<'de, D>(deserializer: D) -> Result<Option<MemoryLimit>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Bytes(u64),
        Text(String),
    }
    let refused = |value: &str| {
        serde::de::Error::custom(format!(
            "memory_limit = {value} is not a number of bytes, \"auto\" or \"none\" (0 means \
             none): the process memory ceiling, e.g. memory_limit = 8589934592 for 8 GiB"
        ))
    };
    match Raw::deserialize(deserializer) {
        Ok(Raw::Bytes(0)) => Ok(Some(MemoryLimit::Unlimited)),
        Ok(Raw::Bytes(bytes)) => Ok(Some(MemoryLimit::Bytes(bytes))),
        Ok(Raw::Text(text)) => text
            .parse::<MemoryLimit>()
            .map(Some)
            .map_err(|_| refused(&format!("{text:?}"))),
        Err(_) => Err(refused("this value")),
    }
}

/// `n` as a size of the script limits (saturating on a 32-bit target).
fn size(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

/// `n` as an integer option value (saturating; TOML integers fit in `i64` anyway).
fn option_value(n: u64) -> ElementProperty {
    ElementProperty::from(i64::try_from(n).unwrap_or(i64::MAX))
}

/// `n` as a traversal limit option: `0` is unlimited (`i64::MAX`), as on the command line.
fn unlimited_at_zero(n: u64) -> ElementProperty {
    if n == 0 {
        ElementProperty::from(i64::MAX)
    } else {
        option_value(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(presets: &SessionPresets) -> Vec<(String, String)> {
        presets
            .options
            .iter()
            .map(|(key, value)| (key.clone(), serde_json::to_string(value).unwrap()))
            .collect()
    }

    #[test]
    fn no_key_is_no_limit() {
        let presets = QueryLimits::new().presets();
        assert!(presets.options.is_empty());
        assert_eq!(presets.script_timeout_ms, None);
        assert_eq!(presets.limits, ScriptLimits::unlimited());
        assert_eq!(QueryLimits::new().summary(), "");
    }

    #[test]
    fn every_key_maps_to_its_preset() {
        let limits: QueryLimits = toml::from_str(
            "timeout_ms = 50\nmax_operations = 1000\nmax_string_size = 64\n\
             max_array_size = 10\nmax_map_size = 11\nmax_traversers = 100\n\
             max_materialized_bytes = 4096\nmax_value_depth = 8",
        )
        .unwrap();
        let presets = limits.presets();
        assert_eq!(presets.script_timeout_ms, Some(50));
        assert_eq!(presets.limits.max_operations, 1000);
        assert_eq!(presets.limits.max_string_size, 64);
        assert_eq!(presets.limits.max_array_size, 10);
        assert_eq!(presets.limits.max_map_size, 11);
        assert_eq!(presets.limits.max_value_depth, 8);
        let options = options(&presets);
        for (key, value) in [
            ("evaluationTimeout", "50"),
            ("traversal.max_traversers", "100"),
            ("traversal.max_materialized_bytes", "4096"),
            ("traversal.max_string_bytes", "64"),
            ("traversal.max_value_depth", "8"),
        ] {
            assert!(
                options.iter().any(|(k, v)| k == key && v.contains(value)),
                "{key}: {options:?}"
            );
        }
        assert_eq!(
            limits.summary(),
            "timeout_ms=50, max_operations=1000, max_string_size=64, max_array_size=10, \
             max_map_size=11, max_traversers=100, max_materialized_bytes=4096, max_value_depth=8"
        );
    }

    #[test]
    fn zero_is_no_limit_and_the_default_depth() {
        let limits = QueryLimits {
            timeout_ms: Some(0),
            max_traversers: Some(0),
            max_value_depth: Some(0),
            ..QueryLimits::new()
        };
        let presets = limits.presets();
        assert_eq!(presets.script_timeout_ms, None);
        let options = options(&presets);
        assert!(!options.iter().any(|(k, _)| k == "evaluationTimeout"));
        assert!(
            !options
                .iter()
                .any(|(k, _)| k == "traversal.max_value_depth")
        );
        let traversers = options
            .iter()
            .find(|(k, _)| k == "traversal.max_traversers")
            .unwrap();
        assert!(
            traversers.1.contains(&i64::MAX.to_string()),
            "{traversers:?}"
        );
    }

    #[test]
    fn a_database_key_wins_over_the_global_one() {
        let global = QueryLimits::new()
            .with_timeout_ms(1000)
            .with_max_traversers(5);
        let database = QueryLimits::new().with_max_traversers(0);
        let merged = database.over(&global);
        assert_eq!(merged.timeout_ms, Some(1000));
        assert_eq!(merged.max_traversers, Some(0));
        assert_eq!(QueryLimits::new().over(&global), global);
        assert_eq!(global.over(&QueryLimits::new()), global);
    }

    #[test]
    fn every_key_is_reachable_by_name() {
        let mut limits = QueryLimits::new();
        for (n, key) in QUERY_LIMIT_KEYS.iter().enumerate() {
            assert_eq!(limits.get(key), Some(None), "{key}");
            assert!(limits.set(key, Some(n as u64 + 1)), "{key}");
            assert_eq!(limits.get(key), Some(Some(n as u64 + 1)), "{key}");
        }
        assert_eq!(
            limits.summary(),
            "timeout_ms=1, max_operations=2, max_string_size=3, max_array_size=4, \
             max_map_size=5, max_traversers=6, max_materialized_bytes=7, max_value_depth=8"
        );
        assert!(limits.set("timeout_ms", None));
        assert_eq!(limits.timeout_ms, None);
        assert_eq!(limits.get("max_traverser"), None);
        assert!(!limits.set("max_traverser", Some(1)));
        let text = toml::to_string(&QueryLimits::new().with_timeout_ms(MAX_LIMIT_VALUE)).unwrap();
        assert_eq!(
            toml::from_str::<QueryLimits>(&text).unwrap().timeout_ms,
            Some(MAX_LIMIT_VALUE)
        );
    }

    #[test]
    fn bad_values_are_refused() {
        for text in [
            "timeout_ms = -1",
            "max_traversers = \"many\"",
            "max_operations = 1.5",
            "max_traverser = 3",
        ] {
            assert!(toml::from_str::<QueryLimits>(text).is_err(), "{text}");
        }
    }

    #[test]
    fn memory_limit_values() {
        #[derive(Deserialize)]
        struct T {
            #[serde(default, deserialize_with = "memory_limit")]
            m: Option<MemoryLimit>,
        }
        let m = |text: &str| toml::from_str::<T>(text).map(|t| t.m);
        assert_eq!(m("").unwrap(), None);
        assert_eq!(m("m = 1024").unwrap(), Some(MemoryLimit::Bytes(1024)));
        assert_eq!(m("m = 0").unwrap(), Some(MemoryLimit::Unlimited));
        assert_eq!(m("m = \"auto\"").unwrap(), Some(MemoryLimit::Auto));
        assert_eq!(m("m = \"None\"").unwrap(), Some(MemoryLimit::Unlimited));
        assert_eq!(m("m = \"4096\"").unwrap(), Some(MemoryLimit::Bytes(4096)));
        for bad in ["m = -1", "m = \"lots\"", "m = true", "m = \"0\""] {
            let err = m(bad).unwrap_err().to_string();
            assert!(err.contains("\"auto\" or \"none\""), "{bad}: {err}");
        }
    }

    #[test]
    fn nothing_set_leaves_the_memory_ceiling_alone() {
        // A process-wide setting: only the no-op is exercised here (other tests run beside).
        apply_memory_ceiling(None, None).unwrap();
    }
}
