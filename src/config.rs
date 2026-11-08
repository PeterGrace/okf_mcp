//! The configuration file (TOML; the design's "Configuration"). Each
//! milestone reads the fields it uses (M7 added `time_server`). Unknown keys
//! are ignored, so one file serves every version of the server.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::Path;
use std::time::Duration;

use bytesize::ByteSize;
use serde::{Deserialize, Deserializer};

use crate::store::catalogs;

use crate::error::OkfError;

/// The default import limit: 64 MiB, uncompressed.
pub const DEFAULT_MAX_IMPORT_BYTES: u64 = 64 * 1024 * 1024;

/// The default listen address: every IPv4 interface, port 8080.
pub const DEFAULT_LISTEN: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 8080));

/// rmcp's default `Host` allow-list (loopback only): right for a local run,
/// wrong behind an ingress, where the config must name the public host.
pub const DEFAULT_ALLOWED_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// How often the server looks for changes made outside it.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// How long an MCP session may sit idle before it is closed.
pub const DEFAULT_SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// The default `get_concept` outline threshold: bodies over 20,000 characters.
pub const DEFAULT_CONCEPT_OUTLINE_CHARS: usize = 20_000;

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
    /// The import route caps the request body at this size plus a little
    /// slack for gzip (`body_limit`).
    pub max_import_bytes: ByteSize,
    /// The address `serve` listens on (`"0.0.0.0:8080"`); `--listen` overrides it.
    pub listen: SocketAddr,
    /// The `Host` names (optionally `host:port`) MCP requests may carry, the
    /// DNS-rebinding check. Behind an ingress, the public hostname.
    pub allowed_hosts: Vec<String>,
    /// The catalog a key with no default of its own reads and writes (design
    /// "Catalog selection", step 4).
    pub default_catalog: Option<String>,
    /// How often to look for imports, catalog and key changes made by the
    /// subcommands (`"10s"`).
    // `with = "humantime_serde"` parses the field through that module's
    // functions instead of `Duration`'s own serde impl (which wants
    // `{secs, nanos}`).
    #[serde(with = "humantime_serde")]
    pub poll_interval: Duration,
    /// How long an MCP session may be idle before it is closed (`"2h"`).
    #[serde(with = "humantime_serde")]
    pub session_idle_timeout: Duration,
    /// The NTP server the clock check measures against (`host`, `host:port`
    /// or an address); none means no check (design "Time").
    pub time_server: Option<String>,
    /// Review cadence overrides keyed by catalog name.
    #[serde(deserialize_with = "deserialize_maintenance_intervals")]
    pub maintenance_review_intervals: BTreeMap<String, Duration>,
    /// `get_concept` returns an outline instead of a body longer than this
    /// many characters (Unicode code points), unless asked for `full`.
    pub concept_outline_chars: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_import_bytes: ByteSize::b(DEFAULT_MAX_IMPORT_BYTES),
            listen: DEFAULT_LISTEN,
            // `Vec::from` takes ownership of the array (after `.map` turns each
            // `&str` into an owned `String`) without copying it a second time.
            allowed_hosts: Vec::from(DEFAULT_ALLOWED_HOSTS.map(String::from)),
            default_catalog: None,
            poll_interval: DEFAULT_POLL_INTERVAL,
            session_idle_timeout: DEFAULT_SESSION_IDLE_TIMEOUT,
            time_server: None,
            maintenance_review_intervals: BTreeMap::new(),
            concept_outline_chars: DEFAULT_CONCEPT_OUTLINE_CHARS,
        }
    }
}

fn deserialize_maintenance_intervals<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct HumanDuration(#[serde(with = "humantime_serde")] Duration);

    let values = BTreeMap::<String, HumanDuration>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .map(|(name, value)| (name, value.0))
        .collect())
}

impl Config {
    /// Review cadence for a catalog, defaulting to 30 days.
    #[must_use]
    pub fn maintenance_review_interval(&self, catalog: &str) -> chrono::Duration {
        let interval = self
            .maintenance_review_intervals
            .get(catalog)
            .copied()
            .unwrap_or(Duration::from_secs(2_592_000));
        chrono::Duration::from_std(interval).expect("validated maintenance interval fits chrono")
    }

    /// Parses a configuration file's text. It does not call
    /// [`Config::validate`]; [`Config::load`] does, so a caller that parses
    /// text itself must validate the result before serving with it.
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

    /// Checks the values serde cannot: ones that would break the server.
    ///
    /// # Returns
    /// `Ok(())`, or the problem in words.
    ///
    /// # Errors
    /// A zero `poll_interval` (tokio's interval panics on zero) or
    /// `session_idle_timeout`, an empty `allowed_hosts` (which rmcp reads as
    /// "accept any `Host`"), or a blank entry in it (which no `Host` matches,
    /// so every request would get 403 with no hint why), or a blank
    /// `time_server` or one with surrounding whitespace (which the lookup
    /// would fail on every check, seen only as a `warn`).
    pub fn validate(&self) -> Result<(), String> {
        if self.poll_interval.is_zero() {
            return Err("poll_interval must be more than zero".to_string());
        }
        if self.session_idle_timeout.is_zero() {
            return Err("session_idle_timeout must be more than zero".to_string());
        }
        if self.allowed_hosts.is_empty() {
            return Err(
                "allowed_hosts is empty; list the host names clients connect to".to_string(),
            );
        }
        // `trim` returns a borrowed slice (`&str`) without the surrounding
        // whitespace, so the check allocates nothing.
        if self.allowed_hosts.iter().any(|host| host.trim().is_empty()) {
            return Err("allowed_hosts has a blank entry; remove it or name a host".to_string());
        }
        // `if let Some(x) = ...`: Python's `if (x := ...) is not None`.
        // `as_deref` turns `&Option<String>` into `Option<&str>`.
        if let Some(server) = self.time_server.as_deref() {
            let trimmed = server.trim();
            if trimmed.is_empty() {
                return Err("time_server is blank; name the NTP server or leave it out".to_string());
            }
            if trimmed != server {
                return Err("time_server has surrounding whitespace; remove it".to_string());
            }
        }
        for (name, interval) in &self.maintenance_review_intervals {
            if !catalogs::is_valid_name(name) {
                return Err(format!(
                    "maintenance_review_intervals: invalid catalog name {name:?}"
                ));
            }
            if *interval < Duration::from_secs(86_400) {
                return Err(format!(
                    "maintenance_review_intervals.{name} must be at least 1 day"
                ));
            }
            if chrono::Duration::from_std(*interval).is_err() {
                return Err(format!(
                    "maintenance_review_intervals.{name} exceeds supported duration"
                ));
            }
        }
        Ok(())
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
    /// [`OkfError::Config`] if the file can't be read or parsed, or fails
    /// [`Config::validate`].
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
        let config = Self::from_toml(&text).map_err(|e| error(e.to_string()))?;
        // `map_err(error)` passes the closure itself: `validate`'s `String`
        // is already the message it wants.
        config.validate().map_err(error)?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_gives_the_defaults() {
        let config = Config::from_toml("").unwrap();

        assert_eq!(config, Config::default());
        assert_eq!(config.max_import_bytes.as_u64(), 64 * 1024 * 1024);
        assert_eq!(config.listen.to_string(), "0.0.0.0:8080");
        assert_eq!(config.allowed_hosts, ["localhost", "127.0.0.1", "::1"]);
        assert_eq!(config.default_catalog, None);
        assert_eq!(config.poll_interval, Duration::from_secs(10));
        assert_eq!(
            config.session_idle_timeout,
            Duration::from_secs(2 * 60 * 60)
        );
        assert_eq!(config.concept_outline_chars, 20_000);
    }

    #[test]
    fn every_field_reads_from_the_file_and_unknown_keys_are_ignored() {
        let text = "max_import_bytes = \"1MiB\"\n\
                    listen = \"127.0.0.1:9000\"\n\
                    allowed_hosts = [\"okf-mcp.example\"]\n\
                    default_catalog = \"pg\"\n\
                    poll_interval = \"90s\"\n\
                    session_idle_timeout = \"30m\"\n\
                    time_server = \"ntp.example\"\n\
                    concept_outline_chars = 500\n\
                    a_later_key = true\n";

        let config = Config::from_toml(text).unwrap();

        assert_eq!(config.max_import_bytes.as_u64(), 1024 * 1024);
        assert_eq!(config.listen.to_string(), "127.0.0.1:9000");
        assert_eq!(config.allowed_hosts, ["okf-mcp.example"]);
        assert_eq!(config.default_catalog.as_deref(), Some("pg"));
        assert_eq!(config.poll_interval, Duration::from_secs(90));
        assert_eq!(config.session_idle_timeout, Duration::from_secs(30 * 60));
        assert_eq!(config.time_server.as_deref(), Some("ntp.example"));
        assert_eq!(config.concept_outline_chars, 500);
    }

    #[test]
    fn time_server_is_optional_and_never_blank() {
        let none = Config::from_toml("").unwrap();
        let named = Config::from_toml("time_server = \"time.example\"\n").unwrap();
        let blank = Config::from_toml("time_server = \"  \"\n").unwrap();
        let padded = Config::from_toml("time_server = \" time.example\\t\"\n").unwrap();

        assert_eq!(none.time_server, None);
        assert_eq!(named.time_server.as_deref(), Some("time.example"));
        assert_eq!(named.validate(), Ok(()));
        assert_eq!(
            blank.validate(),
            Err("time_server is blank; name the NTP server or leave it out".to_string())
        );
        assert_eq!(
            padded.validate(),
            Err("time_server has surrounding whitespace; remove it".to_string())
        );
    }

    #[test]
    fn sizes_take_plain_numbers() {
        let config = Config::from_toml("max_import_bytes = 2048\n").unwrap();
        assert_eq!(config.max_import_bytes.as_u64(), 2048);
    }

    #[test]
    fn a_bad_value_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("okf.toml");
        std::fs::write(&path, "poll_interval = \"soon\"\n").unwrap();

        let error = Config::load(Some(&path)).unwrap_err();

        assert!(matches!(&error, OkfError::Config { path: p, .. } if p.ends_with("okf.toml")));
    }

    #[test]
    fn values_that_would_break_the_server_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("okf.toml");
        for (text, needle) in [
            ("poll_interval = \"0s\"\n", "poll_interval"),
            ("session_idle_timeout = \"0s\"\n", "session_idle_timeout"),
            ("allowed_hosts = []\n", "allowed_hosts"),
            (
                "allowed_hosts = [\"\"]\n",
                "allowed_hosts has a blank entry",
            ),
            (
                "allowed_hosts = [\"localhost\", \"  \"]\n",
                "allowed_hosts has a blank entry",
            ),
        ] {
            std::fs::write(&path, text).unwrap();

            let error = Config::load(Some(&path)).unwrap_err();

            assert!(
                matches!(&error, OkfError::Config { path: p, .. } if p.ends_with("okf.toml")),
                "{text}: {error:?}"
            );
            assert!(error.to_string().contains(needle), "{text}: {error}");
        }
    }

    #[test]
    fn maintenance_review_interval_is_per_catalog() {
        let default = Config::default();
        assert_eq!(
            default.maintenance_review_interval("pg").num_seconds(),
            2_592_000
        );
        let config =
            Config::from_toml("[maintenance_review_intervals]\npg = \"14d\"\nother = \"1d\"\n")
                .unwrap();
        assert_eq!(
            config.maintenance_review_interval("pg").num_seconds(),
            1_209_600
        );
        assert_eq!(
            config.maintenance_review_interval("other").num_seconds(),
            86_400
        );
        assert_eq!(
            config.maintenance_review_interval("missing").num_seconds(),
            2_592_000
        );
        assert_eq!(config.validate(), Ok(()));
        assert!(
            Config::from_toml("[maintenance_review_intervals]\npg = \"not-duration\"\n").is_err()
        );
        let too_short =
            Config::from_toml("[maintenance_review_intervals]\npg = \"86399s\"\n").unwrap();
        assert!(too_short.validate().is_err());
        let invalid_name =
            Config::from_toml("[maintenance_review_intervals]\n\"Bad_Name\" = \"1d\"\n").unwrap();
        assert!(invalid_name.validate().is_err());
    }

    #[test]
    fn no_file_gives_the_defaults() {
        assert_eq!(Config::load(None).unwrap(), Config::default());
    }
}
