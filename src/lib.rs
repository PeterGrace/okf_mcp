//! okf_mcp: an MCP server for Open Knowledge Format (OKF) catalogs.
//!
//! The library holds everything except process startup: the OKF document
//! model (`okf`), catalog archives (`archive`), the per-catalog in-memory
//! snapshots (`catalog`), the database (`db`), API keys (`keys`) and the
//! requests they authenticate (`auth`), which catalogs a call acts on
//! (`selection`), the catalog store (`store`), the configuration file
//! (`config`), the server's shared state and poll loop (`app`), the MCP
//! handler and its tools (`mcp`), the command line (`cli`), errors, and the
//! clock. `main.rs` only starts it.

pub mod app;
pub mod archive;
pub mod auth;
pub mod catalog;
pub mod cli;
pub mod clock;
pub mod config;
pub mod db;
pub mod error;
pub mod keys;
pub mod mcp;
pub mod okf;
pub mod selection;
pub mod store;
