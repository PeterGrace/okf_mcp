//! The configuration file (TOML; the design's "Configuration"). M3 reads only
//! what it uses; later milestones add their fields. Unknown keys are ignored,
//! so one file serves every version of the server.

use std::path::Path;

use bytesize::ByteSize;
use serde::Deserialize;

use crate::error::OkfError;

/// The default import limit: 64 MiB, uncompressed.
pub const DEFAULT_MAX_IMPORT_BYTES: u64 = 64 * 1024 * 1024;

/// Settings read from the configuration file.
// `#[serde(default)]` fills every missing key from `Default::default()`.
// serde ignores keys the struct does not name unless told otherwise
// (`deny_unknown_fields`), which is what lets later milestones' keys through.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The largest uncompressed total an import may expand to (`"64MiB"`, or
    /// a number of bytes). `MiB` is binary; `MB` and `M` are decimal. For a
    /// tarball it counts the whole decompressed tar stream (headers and
    /// 512-byte padding included); for a directory, the files' contents.
    pub max_import_bytes: ByteSize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_import_bytes: ByteSize::b(DEFAULT_MAX_IMPORT_BYTES),
        }
    }
}

impl Config {
    /// Parses a configuration file's text.
    ///
    /// # Arguments
    /// * `text` - the file's contents.
    ///
    /// # Returns
    /// The settings, with defaults for every key the text leaves out.
    ///
    /// # Errors
    /// The TOML parser's error if the text is not valid TOML or a value has
    /// the wrong type.
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Reads the configuration file, or returns the defaults without one.
    ///
    /// # Arguments
    /// * `path` - the file (`--config` or `OKF_MCP_CONFIG`), if any.
    ///
    /// # Returns
    /// The file's settings, or [`Config::default`] when `path` is `None`.
    ///
    /// # Errors
    /// [`OkfError::Config`] if the file can't be read or parsed.
    pub fn load(path: Option<&Path>) -> Result<Self, OkfError> {
        // `let ... else` is a pattern match that must diverge (here `return`)
        // when the pattern does not match: an early return for `None`.
        let Some(path) = path else {
            return Ok(Self::default());
        };
        // A closure (Python's lambda) that wraps a message with the path.
        let error = |message: String| OkfError::Config {
            path: path.display().to_string(),
            message,
        };
        let text = std::fs::read_to_string(path).map_err(|e| error(e.to_string()))?;
        Self::from_toml(&text).map_err(|e| error(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_gives_the_defaults() {
        assert_eq!(Config::from_toml("").unwrap(), Config::default());
        assert_eq!(
            Config::default().max_import_bytes.as_u64(),
            64 * 1024 * 1024
        );
    }

    #[test]
    fn sizes_take_units_or_plain_numbers_and_unknown_keys_are_ignored() {
        let config = Config::from_toml("max_import_bytes = \"1MiB\"\nlisten = \"0.0.0.0:8080\"\n");
        assert_eq!(config.unwrap().max_import_bytes.as_u64(), 1024 * 1024);
        let config = Config::from_toml("max_import_bytes = 2048\n").unwrap();
        assert_eq!(config.max_import_bytes.as_u64(), 2048);
    }

    #[test]
    fn a_bad_value_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("okf.toml");
        std::fs::write(&path, "max_import_bytes = \"lots\"\n").unwrap();

        let error = Config::load(Some(&path)).unwrap_err();

        assert!(matches!(&error, OkfError::Config { path: p, .. } if p.ends_with("okf.toml")));
    }

    #[test]
    fn no_file_gives_the_defaults() {
        assert_eq!(Config::load(None).unwrap(), Config::default());
    }
}
