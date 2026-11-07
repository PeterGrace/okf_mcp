//! Report-only maintenance runner contracts.
//!
//! Offline evaluation and one-shot supervision are library APIs.
//! Configuration checks alone claim neither runnable readiness nor factual verification.

pub mod claude;
pub mod client;
pub mod collect;
pub mod config;
pub mod contract;
pub mod error;
pub mod evaluate;
pub mod journal;
pub mod proposal;
pub mod supervisor;
pub mod types;
