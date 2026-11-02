//! The catalog store: async functions over a Postgres pool, one module per
//! group of tables (design "Storage"). Every function that writes takes `now`
//! from the caller, so every stored timestamp comes from the server clock.

pub mod catalogs;
pub mod documents;
pub mod import;
