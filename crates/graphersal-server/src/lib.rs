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
//! The server has NO access control: it listens on the loopback by default and warns when it
//! listens on any other address.

#![warn(missing_docs)]

pub mod admin;
mod admin_api;
mod client;
pub mod config;
pub mod console;
pub mod database;
pub mod error;
pub mod layout;
pub mod loader;
pub mod lock;
mod mcp;
mod model_archive;
pub mod models;
mod mounts;
pub mod names;
pub mod ops;
pub mod registry;
mod router;
pub mod run_file;
pub mod serve;
#[cfg(test)]
pub(crate) mod test_models;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod text;
mod ui_assets;

pub use admin::TmpBuild;
pub use config::{
    DEFAULT_HEADER_TIMEOUT_SECS, DEFAULT_IDLE_TIMEOUT_SECS, DEFAULT_LISTEN,
    DEFAULT_MAX_CONNECTIONS, DEFAULT_MCP_SESSIONS, DatabaseDefaults, Limits, McpConfig,
    ServerConfig,
};
pub use database::{DatabaseConfig, DatabaseKind, DatabaseRole};
pub use error::ServerError;
pub use layout::{InitReport, ServerDir};
pub use loader::{LoadStart, Loader};
pub use lock::ServerLock;
pub use models::{Adoption, ModelBlob, ModelsDir, ModelsIndex};
pub use ops::{
    Admin, BackupInfo, CreateSpec, Deleted, ExportSpec, Exported, ImportFormat, ImportSource,
    Imported, ModelAddSpec, ModelAdded, ModelRemoved, Restored,
};
pub use registry::{DatabaseEntry, DbState, Registry};
pub use run_file::RunInfo;
pub use serve::{ConnectionLimits, Running, ServeOptions};
