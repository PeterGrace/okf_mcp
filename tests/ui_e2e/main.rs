//! Browser end-to-end tests of the web console (M9), driven by rustwright
//! against the real server. Each journey skips when no Chromium is found
//! (set RUSTWRIGHT_CHROMIUM); `just test-ui` runs them.

#[path = "../support/mod.rs"]
mod support;

mod account;
mod browse;
mod catalogs;
mod changesets;
mod consent;
mod harness;
mod health;
mod keys;
mod login;
mod maintenance;
mod self_service;
mod signin;
mod smoke;
mod teams;
mod users;
