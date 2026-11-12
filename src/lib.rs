//! okf_mcp: an MCP server for Open Knowledge Format (OKF) catalogs.
//!
//! The library holds everything except process startup: the OKF document
//! model (`okf`), catalog archives (`archive`), the per-catalog in-memory
//! snapshots (`catalog`) and the per-connection overlays (`overlay`), the
//! database (`db`), API keys (`keys`) and the requests they authenticate
//! (`auth`), which catalogs a call acts on (`selection`), the catalog store
//! (`store`), the write path every write tool goes through (`write`), the
//! configuration file (`config`), the server's shared state and poll loop
//! (`app`), per-changeset async locks (`keyed_lock`), outgoing mail and its
//! job queue (`mail`), the MCP handler and its tools (`mcp`), the SNTP client (`ntp`),
//! OIDC providers (`oidc`) and the clock check that uses it (`clock_check`), the HTTP router and
//! server (`http`), the OAuth authorization server (`oauth`), the
//! command line (`cli`), errors, and the clock.
//! `main.rs` only starts it.

pub mod app;
pub mod archive;
pub mod auth;
pub mod catalog;
pub mod cli;
pub mod clock;
pub mod clock_check;
pub mod config;
pub mod db;
pub mod error;
pub mod http;
pub mod investigator;
pub mod keyed_lock;
pub mod keys;
pub mod maintenance;
#[cfg(test)]
mod maintenance_contract_tests {
    #[test]
    fn new_id_is_random_hex() {
        let first = crate::maintenance::types::new_id().unwrap();
        assert_eq!(first.len(), 32);
        assert!(first
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_ne!(first, crate::maintenance::types::new_id().unwrap());
    }
}
pub mod mail;
pub mod mcp;
pub mod ntp;
pub mod oauth;
pub mod oidc;
pub mod okf;
pub mod overlay;
pub mod render;
pub mod selection;
pub mod store;
pub mod write;
