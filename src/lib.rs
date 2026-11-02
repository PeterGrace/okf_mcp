//! okf_mcp: an MCP server for Open Knowledge Format (OKF) catalogs.
//!
//! The library holds everything except process startup: the OKF document
//! model (`okf`), catalog archives (`archive`), storage (`db`), errors, and the clock. `main.rs` only wires
//! it together.

pub mod archive;
pub mod clock;
pub mod db;
pub mod error;
pub mod okf;
