//! The OKF document model: a port of `tools/okf.py`'s parsing, editing and
//! path rules. Everything here is pure logic over a [`workspace::Workspace`],
//! so it needs neither git nor a real filesystem in tests.

pub mod analysis;
pub mod doc;
pub mod markdown;
pub mod paths;
pub mod pystr;
pub mod snapshot;
pub mod timestamp;
pub mod workspace;
pub mod yaml;
