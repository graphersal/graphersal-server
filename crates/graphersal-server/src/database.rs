//! `databases/<name>/database.toml`: the configuration of one database.
//!
//! ```toml
//! kind = "layer"                   # "plain" (default) | "layer"
//! role = "normal"                  # "normal" (default) | "base"
//! base = "product-2026.3"          # layer only: the role = "base" database it is layered over
//! auto_load = false                # load at server start
//! read_only = false                # serve read-only (a base is always created read-only)
//! created = "2026-11-22T10:00:00Z" # UTC, set when the database is created
//! description = "Acme's graph"
//! ```
//!
//! Every key is optional (its default applies); an unknown key is an error, so a typo is never
//! ignored silently (as in `server.toml`): a file that does not parse lists its database as
//! `failed` with the reason. The file is written atomically (temporary file + rename).

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::ServerError;
use crate::layout::write_atomic;
use crate::names::validate_name;

/// The file name of a database's configuration.
pub const DATABASE_FILE: &str = "database.toml";
/// The directory of a database's Store, beside its `database.toml`.
pub const STORE_DIR: &str = "store";

/// What a database is made of.
///
/// `#[non_exhaustive]`: further kinds may be added.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DatabaseKind {
    /// A plain Store.
    #[default]
    Plain,
    /// A layer Store over a frozen base (another database, `role = "base"`).
    Layer,
}

/// What a database is for.
///
/// `#[non_exhaustive]`: further roles may be added.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DatabaseRole {
    /// An ordinary database.
    #[default]
    Normal,
    /// A frozen base that layers may be built over: always created read-only, and its
    /// `read_only` cannot be turned off while a layer names it.
    Base,
}

impl DatabaseKind {
    /// The name in `database.toml` (`"plain"`, `"layer"`).
    pub fn as_str(self) -> &'static str {
        match self {
            DatabaseKind::Plain => "plain",
            DatabaseKind::Layer => "layer",
        }
    }
}

impl DatabaseRole {
    /// The name in `database.toml` (`"normal"`, `"base"`).
    pub fn as_str(self) -> &'static str {
        match self {
            DatabaseRole::Normal => "normal",
            DatabaseRole::Base => "base",
        }
    }
}

/// The contents of `database.toml` (module docs).
///
/// `#[non_exhaustive]`: build it with [`DatabaseConfig::plain`] or [`DatabaseConfig::layer`]
/// and the `with_*` setters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct DatabaseConfig {
    /// Plain Store or layer Store.
    #[serde(default)]
    pub kind: DatabaseKind,
    /// Ordinary database or frozen base.
    #[serde(default)]
    pub role: DatabaseRole,
    /// The base database of a layer (`role = "base"`); `None` for a plain database.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Whether the server loads the database when it starts.
    #[serde(default)]
    pub auto_load: bool,
    /// Whether the database is served read-only.
    #[serde(default)]
    pub read_only: bool,
    /// When the database was created (UTC, RFC 3339 such as `2026-11-22T10:00:00Z`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// A free text shown in listings.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

impl DatabaseConfig {
    /// A plain, ordinary database created now.
    pub fn plain() -> Self {
        Self {
            created: Some(utc_now()),
            ..Self::default()
        }
    }

    /// A layer over the base database `base`, created now.
    pub fn layer(base: impl Into<String>) -> Self {
        Self {
            kind: DatabaseKind::Layer,
            base: Some(base.into()),
            created: Some(utc_now()),
            ..Self::default()
        }
    }

    /// The configuration with the role `role`.
    pub fn with_role(mut self, role: DatabaseRole) -> Self {
        self.role = role;
        self
    }

    /// The configuration with `auto_load`.
    pub fn with_auto_load(mut self, auto_load: bool) -> Self {
        self.auto_load = auto_load;
        self
    }

    /// The configuration with `read_only`.
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// The configuration with a description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Parses `database.toml`'s text (`path` names the file in the error) and checks the
    /// configuration on its own ([`DatabaseConfig::check`]).
    pub fn parse(text: &str, path: &Path) -> Result<Self, ServerError> {
        let config: Self = toml::from_str(text).map_err(|err| ServerError::Config {
            path: path.to_path_buf(),
            message: err.message().to_string(),
        })?;
        config.check().map_err(|message| ServerError::Config {
            path: path.to_path_buf(),
            message,
        })?;
        Ok(config)
    }

    /// The rules a configuration follows on its own (the base's existence and role are the
    /// registry's): a layer names a valid base name and is not itself a base; a plain database
    /// names no base. `Err` carries the reason.
    pub fn check(&self) -> Result<(), String> {
        match (self.kind, &self.base) {
            (DatabaseKind::Layer, None) => Err("a layer names its base (base = \"<name>\")".into()),
            (DatabaseKind::Layer, Some(base)) => {
                validate_name(base).map_err(|err| format!("base: {err}"))?;
                if self.role == DatabaseRole::Base {
                    return Err("a layer cannot be a base (role = \"base\" needs kind = \
                                \"plain\")"
                        .into());
                }
                Ok(())
            }
            (DatabaseKind::Plain, Some(_)) => {
                Err("only a layer names a base (kind = \"layer\")".into())
            }
            (DatabaseKind::Plain, None) => Ok(()),
        }
    }

    /// The configuration as `database.toml`'s text.
    pub fn to_toml(&self) -> Result<String, ServerError> {
        toml::to_string(self).map_err(|err| ServerError::Config {
            path: DATABASE_FILE.into(),
            message: err.to_string(),
        })
    }

    /// Reads `<dir>/database.toml`.
    pub fn read(dir: &Path) -> Result<Self, ServerError> {
        let path = dir.join(DATABASE_FILE);
        let text =
            std::fs::read_to_string(&path).map_err(|err| ServerError::io("read", &path, err))?;
        Self::parse(&text, &path)
    }

    /// Writes `<dir>/database.toml` atomically.
    pub fn write(&self, dir: &Path) -> Result<(), ServerError> {
        write_atomic(&dir.join(DATABASE_FILE), self.to_toml()?.as_bytes())
    }
}

/// The current time in UTC as RFC 3339 with seconds (`2026-11-22T10:00:00Z`).
pub fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    utc_text(secs)
}

/// `secs` after the Unix epoch as RFC 3339 UTC (Howard Hinnant's days-to-civil algorithm; no
/// date dependency).
pub(crate) fn utc_text(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX / 2);
    let rest = secs % 86_400;
    let (hour, minute, second) = (rest / 3600, rest % 3600 / 60, rest % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_text_matches_known_instants() {
        assert_eq!(utc_text(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_text(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc_text(1_795_000_000), "2026-11-18T11:06:40Z");
        assert_eq!(utc_now().len(), 20);
    }

    #[test]
    fn round_trip_and_defaults() {
        let path = Path::new(DATABASE_FILE);
        assert_eq!(
            DatabaseConfig::parse("", path).unwrap(),
            DatabaseConfig::default()
        );
        let config = DatabaseConfig::layer("base-1")
            .with_auto_load(true)
            .with_description("a layer");
        let text = config.to_toml().unwrap();
        assert!(text.contains("kind = \"layer\""), "{text}");
        assert!(text.contains("base = \"base-1\""), "{text}");
        assert_eq!(DatabaseConfig::parse(&text, path).unwrap(), config);
        let plain = DatabaseConfig::plain().to_toml().unwrap();
        assert!(!plain.contains("base"), "{plain}");
        assert!(!plain.contains("description"), "{plain}");
    }

    #[test]
    fn refusals() {
        let path = Path::new(DATABASE_FILE);
        for (text, needle) in [
            ("kind = \"layer\"", "names its base"),
            ("base = \"b\"", "only a layer"),
            (
                "kind = \"layer\"\nbase = \"b\"\nrole = \"base\"",
                "cannot be a base",
            ),
            ("kind = \"layer\"\nbase = \"B\"", "invalid name"),
            ("kind = \"other\"", "unknown variant"),
            ("readonly = true", "unknown field"),
            ("auto_load = \"yes\"", "invalid type"),
        ] {
            let err = DatabaseConfig::parse(text, path).unwrap_err();
            assert!(matches!(err, ServerError::Config { .. }), "{text}: {err}");
            assert!(err.to_string().contains(needle), "{text}: {err}");
        }
    }
}
