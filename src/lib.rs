//! okf_mcp: an MCP server for Open Knowledge Format (OKF) catalogs.
//!
//! The library holds everything except process startup: the OKF document
//! model (`okf`), catalog archives (`archive`), the database (`db`), API
//! keys (`keys`), the catalog store (`store`), the configuration file
//! (`config`), the command line (`cli`), errors, and the clock. `main.rs` only starts it.

pub mod archive;
pub mod cli;
pub mod clock;
pub mod config;
pub mod db;
pub mod error;
pub mod keys;
pub mod okf;
pub mod store;
