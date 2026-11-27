//! **EXPERIMENTAL**: `graphersal-server`, one process serving many Graphersal graph databases
//! from one server directory over HTTP.
//!
//! This version holds the server directory ([`ServerDir`], `init`), its configuration
//! ([`ServerConfig`], `server.toml`), the server lock ([`ServerLock`]), the run file
//! ([`RunInfo`], `server.run`), the database registry ([`Registry`]: `databases/*/database.toml`,
//! `aliases.toml`, names, states, the scan at start, the admin core in [`admin`]), the loader
//! ([`Loader`]: background loads into served contexts, the shared pool of layer bases,
//! unload, the runtime read-only toggle, `auto_load` at start) and the HTTP listener
//! ([`serve::start`]) with the health probes, the loaded databases under `/db/<name>/` (aliases
//! resolved per request, `409` for a database that is not loaded), the server's own UI at `/`
//! (the embedded playground showing the `server` UI profile: its Databases, Backups, Import,
//! Export and Models tabs, served from `/server-ui/`), `/api/info` and
//! the admin API (`/api/admin/...`: list, create, delete, clone, load, unload, read-only,
//! auto-load, aliases, backup, backups, restore, export, import, models) over ONE admin core ([`ops::Admin`]) that the console's `db` and `models` commands
//! ([`console`]) use too, on the directory itself or through the running server's API, and
//! the shared embedding models ([`models`]: one copy of every model blob in `models/blobs/`,
//! hard-linked into the databases' Stores), and the MCP endpoints: `/mcp` over every database
//! of the registry (`list_graphs`, `use_graph`, an optional `graph` per call) and
//! `/db/<name>/mcp` per loaded database, one shared endpoint (`server.toml` `[mcp]`).
//!
//! Access control ([`access`]): `access.toml` (users, token hashes, grants), the principal of a
//! request from the authenticator chain, and the level per database; a server directory
//! without `access.toml` is closed (`init` and the first `serve` create the first admin and
//! print its token once). Every administrative event goes through the audit sink
//! ([`audit`]). The router enforces them on every route but the probes, the root `/api/info`
//! and the UI files: the admin API (Runs included) for admins only, each database by the
//! principal's level (none = it does not exist for them, read = read-only requests), `/mcp`
//! per principal; `POST /api/login` / `POST /api/logout` for browsers (the `gs_session`
//! cookie). Browser and MCP sessions survive a restart ([`sessions`]: hashes only, in the
//! server directory's `sessions/`). The server listens on the loopback by default and warns when it listens on any
//! other address (tokens travel in clear text over plain HTTP).

#![warn(missing_docs)]

pub mod access;
pub mod admin;
mod admin_api;
mod archive;
pub mod audit;
mod client;
pub mod config;
pub mod console;
mod control;
pub mod daemon;
pub mod database;
pub mod error;
pub mod layout;
pub mod limits;
pub mod loader;
pub mod lock;
mod mcp;
mod mcp_control;
mod mcp_guidance;
mod mcp_profiles;
mod model_archive;
mod model_download;
pub mod models;
mod mounts;
pub mod names;
pub mod openapi;
pub mod ops;
pub mod recovery;
pub mod registry;
pub mod revision;
mod router;
pub mod run_file;
pub mod security;
pub mod serve;
pub mod server_backup;
pub mod server_settings;
pub mod sessions;
mod settings;
#[cfg(test)]
pub(crate) mod test_models;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod text;
mod ui_assets;
mod usage;
mod user_api;

pub use access::{Access, FirstAdmin, Level, Principal, PrincipalKind};
pub use admin::{Renamed, TmpBuild};
pub use audit::{Audit, AuditEvent, AuditSink};
pub use config::{
    AccessConfig, DEFAULT_HEADER_TIMEOUT_SECS, DEFAULT_IDLE_TIMEOUT_SECS, DEFAULT_LISTEN,
    DEFAULT_MAX_CONNECTIONS, DEFAULT_MCP_SESSIONS, DEFAULT_MCP_SESSIONS_PER_IDENTITY,
    DEFAULT_SESSION_IDLE_TIMEOUT_SECS, DEFAULT_SESSION_LIFETIME_SECS, DatabaseDefaults, HttpConfig,
    Limits, McpConfig, PublicScheme, ServerConfig, SessionsConfig,
};
pub use database::{DatabaseConfig, DatabaseKind, DatabaseRole};
pub use error::ServerError;
pub use layout::{InitReport, ServerDir};
pub use limits::{DatabaseLimits, QueryLimits};
pub use loader::{LoadStart, Loader};
pub use lock::ServerLock;
pub use models::{Adoption, ModelBlob, ModelsDir, ModelsIndex};
pub use ops::{
    Admin, BackupInfo, CreateSpec, Deleted, DiscardedChanges, ExportSpec, Exported, ImportFormat,
    ImportSource, Imported, LayerResetDone, ModelAddSpec, ModelAdded, ModelDownloadSpec,
    ModelRemoved, Restored,
};
pub use registry::{DatabaseEntry, DbState, Registry};
pub use run_file::RunInfo;
pub use serve::{ConnectionLimits, Running, ServeOptions, StopHook};
