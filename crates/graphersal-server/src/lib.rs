//! **EXPERIMENTAL**: `graphersal-server`, one process serving many Graphersal graph databases
//! from one server directory over HTTP.
//!
//! This version holds the server directory ([`ServerDir`], `init`), its configuration
//! ([`ServerConfig`], `server.toml`), the server lock ([`ServerLock`]), the run file
//! ([`RunInfo`], `server.run`), the database registry ([`Registry`]: `databases/*/database.toml`,
//! `aliases.toml`, names, states, the scan at start, the admin core in [`admin`]), the loader
//! ([`Loader`]: background loads into served contexts, the shared pool of layer bases,
//! unload, the runtime read-only toggle, `auto_load` at start) and the HTTP listener
//! ([`serve::start`]) with the health probes, a placeholder `/` and `/api/info`. The HTTP
//! routes of the databases, the admin API and MCP come in later versions.
//!
//! The server has NO access control: it listens on the loopback by default and warns when it
//! listens on any other address.

#![warn(missing_docs)]

pub mod admin;
pub mod config;
pub mod database;
pub mod error;
pub mod layout;
pub mod loader;
pub mod lock;
pub mod names;
pub mod registry;
mod router;
pub mod run_file;
pub mod serve;
#[cfg(test)]
pub(crate) mod test_support;

pub use admin::TmpBuild;
pub use config::{DEFAULT_LISTEN, DatabaseDefaults, McpConfig, ServerConfig};
pub use database::{DatabaseConfig, DatabaseKind, DatabaseRole};
pub use error::ServerError;
pub use layout::{InitReport, ServerDir};
pub use loader::{LoadStart, Loader};
pub use lock::ServerLock;
pub use registry::{DatabaseEntry, DbState, Registry};
pub use run_file::RunInfo;
pub use serve::{Running, ServeOptions};
