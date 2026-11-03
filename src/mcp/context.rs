//! What one tool call acts as and on: the principal, the connection's
//! catalogs, and one view of the catalog map.

use std::sync::Arc;

use axum::http::request::Parts;

use crate::app::{App, Catalogs};
use crate::auth::Principal;
use crate::catalog::CatalogState;
use crate::error::OkfError;
use crate::selection::{connection_catalogs, resolve, Selection, Sources};

/// One tool call's context.
// `Arc<T>` is a shared, reference-counted pointer (like holding a Python
// reference): cloning one bumps a counter, it never copies `T`.
pub struct ToolContext {
    app: Arc<App>,
    principal: Arc<Principal>,
    connection: Option<Vec<String>>,
    // Taken once per call, so the names `select` checks are the states the
    // tool then reads, even if the poll loop swaps the map meanwhile.
    catalogs: Arc<Catalogs>,
}

impl ToolContext {
    /// A context for `principal` on a connection naming `connection`.
    ///
    /// # Arguments
    /// * `app` - the shared state.
    /// * `principal` - the calling key.
    /// * `connection` - the connection's catalogs (`X-OKF-Catalogs` or
    ///   `?catalogs=`), if it names any.
    ///
    /// # Returns
    /// The context, over the catalog map as of now.
    pub fn new(app: Arc<App>, principal: Arc<Principal>, connection: Option<Vec<String>>) -> Self {
        let catalogs = app.catalogs();
        Self {
            app,
            principal,
            connection,
            catalogs,
        }
    }

    /// The context of the HTTP request rmcp handed the tool.
    ///
    /// # Arguments
    /// * `app` - the shared state.
    /// * `parts` - the request: its extensions hold the principal the auth
    ///   layer inserted, its header or query the connection's catalogs.
    ///
    /// # Returns
    /// The context.
    ///
    /// # Errors
    /// [`OkfError::Forbidden`] if the request carries no principal, which
    /// only a router without the auth layer would allow.
    pub fn from_parts(app: &Arc<App>, parts: &Parts) -> Result<Self, OkfError> {
        // Extensions are a map keyed by type: `get::<Arc<Principal>>()`
        // finds the value the auth middleware inserted, if any.
        let principal = parts
            .extensions
            .get::<Arc<Principal>>()
            .cloned()
            .ok_or_else(|| OkfError::Forbidden {
                reason: "the request carries no API key".to_string(),
            })?;
        Ok(Self::new(
            Arc::clone(app),
            principal,
            connection_catalogs(parts),
        ))
    }

    /// The shared state.
    ///
    /// # Returns
    /// The app this call runs in.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// The calling key.
    ///
    /// # Returns
    /// Its principal.
    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    /// The catalog map this call sees.
    ///
    /// # Returns
    /// Every loaded catalog, by name, as of the context's creation.
    pub fn catalogs(&self) -> &Catalogs {
        &self.catalogs
    }

    /// Resolves the call's catalogs (design "Catalog selection").
    ///
    /// # Arguments
    /// * `argument` - the call's `catalogs` (or `catalog` as one name).
    ///
    /// # Returns
    /// The selection, every name in this call's catalog map.
    ///
    /// # Errors
    /// As [`resolve`].
    pub fn select(&self, argument: Option<&[String]>) -> Result<Selection, OkfError> {
        let sources = Sources {
            argument,
            connection: self.connection.as_deref(),
            server_default: self.app.config().default_catalog.as_deref(),
        };
        // The closure borrows `self.catalogs`, so `resolve` checks names
        // against the same map `states` then reads.
        resolve(&self.principal, sources, |name| {
            self.catalogs.contains_key(name)
        })
    }

    /// The states of the selected catalogs, in order.
    ///
    /// # Arguments
    /// * `argument` - as [`ToolContext::select`].
    ///
    /// # Returns
    /// One state per selected catalog; never empty.
    ///
    /// # Errors
    /// As [`ToolContext::select`].
    pub fn states(&self, argument: Option<&[String]>) -> Result<Vec<Arc<CatalogState>>, OkfError> {
        let selection = self.select(argument)?;
        Ok(selection
            .catalogs()
            .iter()
            .map(|name| self.state(name))
            .collect())
    }

    /// The read set of a tool with a single `catalog` argument: that catalog,
    /// else the selected catalogs (Decision 1).
    ///
    /// # Arguments
    /// * `catalog` - the call's `catalog` argument.
    ///
    /// # Returns
    /// The states, in order; never empty.
    ///
    /// # Errors
    /// As [`ToolContext::select`].
    pub fn read_set(&self, catalog: Option<&str>) -> Result<Vec<Arc<CatalogState>>, OkfError> {
        // `select` takes a slice of `String`s, so the name is copied into one
        // `String` (a small allocation); `std::slice::from_ref` then views it
        // as a one-element slice without also allocating a `Vec`.
        let argument = catalog.map(str::to_string);
        self.states(argument.as_ref().map(std::slice::from_ref))
    }

    /// The one catalog a report runs on: `catalog`, else the first selected.
    ///
    /// # Arguments
    /// * `catalog` - the call's `catalog` argument.
    ///
    /// # Returns
    /// That catalog's state.
    ///
    /// # Errors
    /// As [`ToolContext::select`].
    pub fn one(&self, catalog: Option<&str>) -> Result<Arc<CatalogState>, OkfError> {
        let argument = catalog.map(str::to_string);
        let selection = self.select(argument.as_ref().map(std::slice::from_ref))?;
        // `write_target` is the first selected catalog.
        Ok(self.state(selection.write_target()))
    }

    /// The state of a catalog `select` returned.
    fn state(&self, name: &str) -> Arc<CatalogState> {
        let state = self
            .catalogs
            .get(name)
            .expect("select only returns names in this call's map");
        Arc::clone(state)
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::mcp::testing::{app_with, context, principal};

    // `Arc<CatalogState>` to its name, for asserts.
    fn names(states: &[std::sync::Arc<crate::catalog::CatalogState>]) -> Vec<&str> {
        states.iter().map(|state| state.name()).collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_argument_wins_and_reports_use_the_first_selected_catalog() {
        let config = Config {
            default_catalog: Some("home".to_string()),
            ..Config::default()
        };
        let test = app_with(&[("pg", &[]), ("work", &[]), ("home", &[])], config).await;
        let on_connection = context(&test, principal(&[], None), Some(&["work", "pg"]));
        let on_default = context(&test, principal(&["pg", "home"], None), None);
        let argument = ["pg".to_string()];

        let states = on_connection.states(None).unwrap();
        let chosen = on_connection.states(Some(&argument)).unwrap();
        let read_set = on_connection.read_set(Some("home")).unwrap();
        let first = on_connection.one(None).unwrap();
        let fallback = on_default.read_set(None).unwrap();
        let unreachable = on_default.one(Some("work")).unwrap_err();

        assert_eq!(names(&states), ["work", "pg"]);
        assert_eq!(names(&chosen), ["pg"]);
        assert_eq!(names(&read_set), ["home"]);
        assert_eq!(first.name(), "work");
        assert_eq!(names(&fallback), ["home"]);
        assert_eq!(unreachable.kind(), "CatalogNotSelected");
    }
}
