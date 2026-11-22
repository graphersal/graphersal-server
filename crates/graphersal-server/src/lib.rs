//! **EXPERIMENTAL**: `graphersal-server`, one process serving many Graphersal graph databases
//! from one server directory over HTTP.
//!
//! This version is the skeleton: the server directory ([`ServerDir`], `init`), its
//! configuration ([`ServerConfig`], `server.toml`), the server lock ([`ServerLock`]), the run
//! file ([`RunInfo`], `server.run`) and the HTTP listener ([`serve::start`]) with the health
//! probes, a placeholder `/` and `/api/info`. Databases, the registry, the admin API and MCP
//! come in later versions.
//!
//! The server has NO access control: it listens on the loopback by default and warns when it
//! listens on any other address.

#![warn(missing_docs)]

pub mod config;
pub mod error;
pub mod layout;
pub mod lock;
mod router;
pub mod run_file;
pub mod serve;
#[cfg(test)]
pub(crate) mod test_support;

pub use config::{DEFAULT_LISTEN, DatabaseDefaults, McpConfig, ServerConfig};
pub use error::ServerError;
pub use layout::{InitReport, ServerDir};
pub use lock::ServerLock;
pub use run_file::RunInfo;
pub use serve::{Running, ServeOptions};
