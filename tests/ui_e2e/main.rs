//! Browser end-to-end tests of the web console (M9), driven by rustwright
//! against the real server. Each journey skips when no Chromium is found
//! (set RUSTWRIGHT_CHROMIUM); `just test-ui` runs them.

#[path = "../support/mod.rs"]
mod support;

mod catalogs;
mod harness;
mod health;
mod keys;
mod login;
// Tasks 4, 6 and 7 add: mod browse; mod maintenance; mod changesets;
