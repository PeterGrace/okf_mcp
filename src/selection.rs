//! Catalog selection (design "Catalog selection"): which catalogs a tool
//! call reads, and which one it would write.
//!
//! The sources, in order: the call's argument; the connection (the
//! `X-OKF-Catalogs` header, else `?catalogs=` on the MCP URL); the key's
//! default catalog; the server's `default_catalog`. The first that is set
//! wins, and every name in it must exist and be reachable by the key.

use axum::http::request::Parts;

use crate::auth::Principal;
use crate::error::OkfError;

/// The header naming a connection's catalogs (`pg,work`); HTTP header names
/// are case-insensitive, and `http` stores them lowercase.
pub const CATALOGS_HEADER: &str = "x-okf-catalogs";

/// The MCP URL's query parameter with the same meaning.
pub const CATALOGS_QUERY: &str = "catalogs";

/// The resolved catalogs of one call: never empty, no duplicates, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    catalogs: Vec<String>,
}

impl Selection {
    /// The read set, in order.
    pub fn catalogs(&self) -> &[String] {
        &self.catalogs
    }

    /// The default write target: the first catalog.
    pub fn write_target(&self) -> &str {
        // `resolve` never builds an empty selection.
        &self.catalogs[0]
    }
}

/// Where a call's catalogs can come from, before the key's default.
// `Copy`: three borrowed fields, so tests can build variants with `..base`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sources<'a> {
    /// The tool call's `catalog` (as a one-name list) or `catalogs`.
    pub argument: Option<&'a [String]>,
    /// The connection's list ([`connection_catalogs`]).
    pub connection: Option<&'a [String]>,
    /// The server's `default_catalog`.
    pub server_default: Option<&'a str>,
}

/// Splits a comma-separated list, trimming names and dropping empty ones.
// `impl Iterator<...> + '_`: a lazy sequence (like a Python generator) that
// borrows `list`, so nothing is copied until a caller collects it.
fn split_names(list: &str) -> impl Iterator<Item = String> + '_ {
    list.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

/// `None` for an empty list, so an empty source counts as unset.
fn non_empty(names: Vec<String>) -> Option<Vec<String>> {
    // `then_some(x)`: `Some(x)` if the condition holds, else `None`.
    (!names.is_empty()).then_some(names)
}

/// The catalogs a connection names: the `X-OKF-Catalogs` header, else the
/// `catalogs` query parameter.
///
/// Repeated headers are read in order, as one list (RFC 9110 5.3), and so
/// are repeated `catalogs` parameters (`?catalogs=pg&catalogs=work`). A header
/// that names nothing (`" , "`) counts as unset, so the query is read. Bytes
/// that are not UTF-8 are kept (lossily), so a garbled name reaches
/// [`resolve`] and fails naming it rather than being dropped.
///
/// Names are kept exactly as given, and catalog names are matched
/// case-sensitively, so `PG` does not select `pg`. In the query, `+` decodes
/// as a space: `?catalogs=pg+work` is the one name `"pg work"`. Separate
/// names with `,` (or `%2C`). Either mistake fails in [`resolve`], naming
/// the name it could not find.
///
/// # Arguments
/// * `parts` - the HTTP request (rmcp passes it to every tool).
///
/// # Returns
/// The names, or `None` if neither source names a catalog.
pub fn connection_catalogs(parts: &Parts) -> Option<Vec<String>> {
    let mut from_header: Vec<String> = Vec::new();
    for value in parts.headers.get_all(CATALOGS_HEADER) {
        // `from_utf8_lossy` returns a `Cow`: a borrow when the bytes are
        // valid UTF-8, a new string (with U+FFFD for bad bytes) otherwise.
        from_header.extend(split_names(&String::from_utf8_lossy(value.as_bytes())));
    }
    // `or_else` runs the closure only when the header named nothing.
    non_empty(from_header).or_else(|| {
        let query = parts.uri.query()?;
        let mut from_query: Vec<String> = Vec::new();
        // `parse` yields decoded `(key, value)` pairs (`%2C` becomes `,`).
        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            if key == CATALOGS_QUERY {
                from_query.extend(split_names(&value));
            }
        }
        non_empty(from_query)
    })
}

/// Resolves a call's catalogs.
///
/// # Arguments
/// * `principal` - the calling key.
/// * `sources` - the argument, the connection and the server default.
/// * `exists` - whether a catalog of that name is loaded.
///
/// # Returns
/// The selection: the first source that is set, without duplicates.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] naming the first catalog that does not
/// exist or that the key does not reach; [`OkfError::Refused`] if no source
/// names a catalog.
// `impl Fn(&str) -> bool`: any function or closure taking a name, like a
// Python callable argument.
pub fn resolve(
    principal: &Principal,
    sources: Sources<'_>,
    exists: impl Fn(&str) -> bool,
) -> Result<Selection, OkfError> {
    // `as_deref`: `Option<String>` to `Option<&str>`, borrowing the name.
    let key_default = principal.default_catalog.as_deref();
    // The argument, else the connection. An empty list (`catalogs: []`)
    // counts as unset, so the selection is never empty; `flatten` skips the
    // sources that are `None`.
    let listed = [sources.argument, sources.connection]
        .into_iter()
        .flatten()
        .find(|list| !list.is_empty());
    let names: Vec<&str> = if let Some(list) = listed {
        list.iter().map(String::as_str).collect()
    } else if let Some(name) = key_default.or(sources.server_default) {
        vec![name]
    } else {
        return Err(OkfError::Refused {
            reason: "no catalog selected: pass `catalog`, set the X-OKF-Catalogs header \
                     or `?catalogs=` on the MCP URL, or give the key a default catalog"
                .to_string(),
        });
    };
    let mut catalogs: Vec<String> = Vec::with_capacity(names.len());
    for name in names {
        if !exists(name) || !principal.reaches(name) {
            return Err(OkfError::CatalogNotSelected {
                catalog: name.to_string(),
            });
        }
        // A few names at most, so a linear `contains` beats a set.
        if !catalogs.iter().any(|seen| seen == name) {
            catalogs.push(name.to_string());
        }
    }
    Ok(Selection { catalogs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    use crate::auth::Reach;
    use crate::store::keys::Access;

    fn principal(reach: &[&str], default: Option<&str>) -> Principal {
        Principal {
            key_id: "k".to_string(),
            access: Access::Read,
            reach: if reach.is_empty() {
                Reach::All
            } else {
                Reach::Catalogs(reach.iter().map(|name| name.to_string()).collect())
            },
            default_catalog: default.map(str::to_string),
            actor: "a".to_string(),
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    const EXISTING: [&str; 3] = ["pg", "work", "home"];

    fn exists(name: &str) -> bool {
        EXISTING.contains(&name)
    }

    fn select(principal: &Principal, sources: Sources<'_>) -> Result<Vec<String>, OkfError> {
        resolve(principal, sources, exists).map(|selection| selection.catalogs().to_vec())
    }

    #[test]
    fn the_first_source_that_is_set_wins() {
        let key = principal(&[], Some("home"));
        let argument = names(&["work"]);
        let connection = names(&["pg", "work"]);
        let all = Sources {
            argument: Some(&argument),
            connection: Some(&connection),
            server_default: Some("pg"),
        };

        assert_eq!(select(&key, all).unwrap(), ["work"]);
        let no_argument = Sources {
            argument: None,
            ..all
        };
        assert_eq!(select(&key, no_argument).unwrap(), ["pg", "work"]);
        let key_default = Sources {
            connection: None,
            ..no_argument
        };
        assert_eq!(select(&key, key_default).unwrap(), ["home"]);
        let server_default = principal(&[], None);
        assert_eq!(select(&server_default, key_default).unwrap(), ["pg"]);
    }

    #[test]
    fn an_empty_argument_list_counts_as_unset() {
        let key = principal(&[], None);
        let empty: Vec<String> = Vec::new();
        let connection = names(&["work"]);
        let sources = Sources {
            argument: Some(&empty),
            connection: Some(&connection),
            server_default: None,
        };

        assert_eq!(select(&key, sources).unwrap(), ["work"]);
        let empty_connection = Sources {
            connection: Some(&empty),
            server_default: Some("pg"),
            ..sources
        };
        assert_eq!(select(&key, empty_connection).unwrap(), ["pg"]);
    }

    #[test]
    fn the_first_name_is_the_write_target_and_duplicates_collapse() {
        let key = principal(&[], None);
        let connection = names(&["work", "pg", "work"]);
        let sources = Sources {
            argument: None,
            connection: Some(&connection),
            server_default: None,
        };

        let selection = resolve(&key, sources, exists).unwrap();

        assert_eq!(selection.catalogs(), ["work", "pg"]);
        assert_eq!(selection.write_target(), "work");
    }

    #[test]
    fn an_unknown_or_ungranted_catalog_fails_naming_it() {
        let key = principal(&["pg"], None);
        for list in [&["pg", "nope"][..], &["work"][..]] {
            let connection = names(list);
            let sources = Sources {
                argument: None,
                connection: Some(&connection),
                server_default: None,
            };

            let error = select(&key, sources).unwrap_err();

            assert!(
                matches!(&error, OkfError::CatalogNotSelected { catalog } if catalog == list[list.len() - 1]),
                "{list:?}: {error}"
            );
        }
    }

    #[test]
    fn an_ungranted_server_default_is_an_error_too() {
        let key = principal(&["work"], None);
        let sources = Sources {
            argument: None,
            connection: None,
            server_default: Some("pg"),
        };

        assert!(matches!(
            select(&key, sources),
            Err(OkfError::CatalogNotSelected { catalog }) if catalog == "pg"
        ));
    }

    #[test]
    fn nothing_set_means_nothing_selected() {
        let key = principal(&[], None);
        let sources = Sources {
            argument: None,
            connection: None,
            server_default: None,
        };

        let error = select(&key, sources).unwrap_err();

        assert!(matches!(error, OkfError::Refused { .. }));
        assert!(error.to_string().contains("X-OKF-Catalogs"));
        assert!(error.to_string().contains("?catalogs="));
    }

    fn parts(uri: &str, header: Option<&str>) -> Parts {
        let mut request = axum::http::Request::builder().uri(uri);
        if let Some(value) = header {
            request = request.header(CATALOGS_HEADER, value);
        }
        request.body(()).unwrap().into_parts().0
    }

    #[test]
    fn the_connection_reads_the_header_before_the_query() {
        assert_eq!(
            connection_catalogs(&parts("/mcp?catalogs=home", Some(" pg, work ,"))),
            Some(names(&["pg", "work"]))
        );
        assert_eq!(
            connection_catalogs(&parts("/mcp?x=1&catalogs=home%2Cpg", None)),
            Some(names(&["home", "pg"]))
        );
        assert_eq!(
            connection_catalogs(&parts("/mcp?catalogs=home", Some(" , "))),
            Some(names(&["home"]))
        );
        assert_eq!(connection_catalogs(&parts("/mcp", Some(" , "))), None);
        assert_eq!(connection_catalogs(&parts("/mcp", None)), None);
        assert_eq!(
            connection_catalogs(&parts("/mcp?catalogs=pg&catalogs=work", None)),
            Some(names(&["pg", "work"]))
        );
        assert_eq!(
            connection_catalogs(&parts("/mcp?catalogs=&catalogs=pg", None)),
            Some(names(&["pg"]))
        );

        let mut repeated = parts("/mcp?catalogs=home", Some("pg"));
        repeated
            .headers
            .append(CATALOGS_HEADER, HeaderValue::from_static("work"));
        assert_eq!(connection_catalogs(&repeated), Some(names(&["pg", "work"])));

        let mut garbled = parts("/mcp?catalogs=home", None);
        garbled.headers.insert(
            CATALOGS_HEADER,
            HeaderValue::from_bytes(b"pg,w\xffrk").unwrap(),
        );
        assert_eq!(
            connection_catalogs(&garbled),
            Some(names(&["pg", "w\u{fffd}rk"]))
        );
    }
}
