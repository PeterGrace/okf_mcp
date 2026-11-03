//! What one tool call acts as and on: the principal, the connection's
//! catalogs and changeset label, and one view of the catalog map.

use std::sync::Arc;

use axum::http::request::Parts;

use crate::app::{App, Catalogs};
use crate::auth::Principal;
use crate::catalog::CatalogState;
use crate::error::OkfError;
use crate::overlay::View;
use crate::selection::{
    connection_catalogs, connection_label, resolve, Selection, Sources, DEFAULT_LABEL,
};
use crate::store::changesets::{self, ChangesetKey};
use crate::store::keys::Access;

/// Is this [`ToolContext::view`] error the connection's overlay failing to
/// apply (its entries clash with the published files), rather than the
/// database failing?
///
/// A read across several catalogs leaves such a catalog out and carries on;
/// a database error (internal) or [`OkfError::Busy`] fails the call, so the
/// agent is told to retry instead of seeing fewer results.
///
/// # Arguments
/// * `error` - the error `view` returned.
///
/// # Returns
/// `true` for an overlay that does not apply.
pub fn is_broken_overlay(error: &OkfError) -> bool {
    !error.is_internal() && !matches!(error, OkfError::Busy { .. })
}

/// One tool call's context.
// `Arc<T>` is a shared, reference-counted pointer (like holding a Python
// reference): cloning one bumps a counter, it never copies `T`.
pub struct ToolContext {
    app: Arc<App>,
    principal: Arc<Principal>,
    connection: Option<Vec<String>>,
    // The connection's changeset label (M5 Decision 1).
    label: String,
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
            label: DEFAULT_LABEL.to_string(),
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
        Ok(
            Self::new(Arc::clone(app), principal, connection_catalogs(parts))
                .with_label(connection_label(parts)),
        )
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

    /// The connection's changeset label (M5 Decision 1).
    ///
    /// # Returns
    /// The label: from the request in [`ToolContext::from_parts`],
    /// [`DEFAULT_LABEL`] from [`ToolContext::new`].
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The same context on another label.
    ///
    /// # Arguments
    /// * `label` - the label; `impl Into<String>` takes a `&str` or a `String`.
    ///
    /// # Returns
    /// The context, consumed and returned (`self` by value), so calls chain.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
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

    /// What this call reads of `state`: this key and label's overlay if they
    /// have an open changeset in the catalog, else the published state.
    ///
    /// One lookup in the open-changeset index per call and catalog. A `read`
    /// key can never write, so it never has a changeset and skips it.
    ///
    /// # Arguments
    /// * `state` - a catalog this call selected.
    ///
    /// # Returns
    /// The view.
    ///
    /// # Errors
    /// [`OkfError::Database`]; an overlay build error
    /// ([`crate::overlay::build_overlay`]).
    pub async fn view(&self, state: &CatalogState) -> Result<View, OkfError> {
        // `Access` is ordered (`Read < Write < Admin`), like an `IntEnum`.
        if self.principal.access < Access::Write {
            return Ok(View::Published(state.current()));
        }
        let key = ChangesetKey {
            catalog_id: state.id(),
            key_id: &self.principal.key_id,
            label: &self.label,
        };
        match changesets::find_open(self.app.pool(), &key).await? {
            None => Ok(View::Published(state.current())),
            Some(changeset) => {
                let overlay = self
                    .app
                    .overlays()
                    .get(self.app.pool(), state, &changeset)
                    .await?;
                Ok(View::Pending(overlay))
            }
        }
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
    use chrono::Utc;

    use crate::config::Config;
    use crate::mcp::testing::{
        app_with, context, pending_entry, principal, principal_as, store_key, PendingEdit,
    };
    use crate::selection::{CHANGESET_HEADER, DEFAULT_LABEL};
    use crate::store::keys::Access;

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

    #[tokio::test(flavor = "multi_thread")]
    async fn the_context_takes_its_label_from_the_request() {
        let test = app_with(&[("pg", &[])], Config::default()).await;
        let mut request = axum::http::Request::builder()
            .uri("/mcp")
            .header(CHANGESET_HEADER, "drafts")
            .body(())
            .unwrap();
        request.extensions_mut().insert(principal(&[], Some("pg")));
        let (parts, _) = request.into_parts();

        let from_request = super::ToolContext::from_parts(&test.app, &parts).unwrap();
        let plain = context(&test, principal(&[], Some("pg")), None);
        let relabelled = context(&test, principal(&[], Some("pg")), None).with_label("other");

        assert_eq!(from_request.label(), "drafts");
        assert_eq!(plain.label(), DEFAULT_LABEL);
        assert_eq!(relabelled.label(), "other");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_connection_s_key_and_label_pick_its_changeset() {
        let test = app_with(&[("pg", &[])], Config::default()).await;
        store_key(&test, "alice").await;
        store_key(&test, "bob").await;
        let alice = || {
            context(
                &test,
                principal_as("alice", Access::Write, &[], Some("pg")),
                None,
            )
        };
        let bob = context(
            &test,
            principal_as("bob", Access::Write, &[], Some("pg")),
            None,
        );
        let edit = PendingEdit {
            catalog: "pg",
            path: "a.md",
            text: Some("# A\n"),
            at: Utc::now(),
        };

        let first = pending_entry(&test, &alice(), &edit).await;
        let again = pending_entry(&test, &alice(), &edit).await;
        let drafts = pending_entry(&test, &alice().with_label("drafts"), &edit).await;
        let other_key = pending_entry(&test, &bob, &edit).await;

        assert_eq!(again.id, first.id);
        assert_eq!(again.version, 2);
        assert_eq!(drafts.label, "drafts");
        assert_ne!(drafts.id, first.id);
        assert_eq!(other_key.key_id.as_deref(), Some("bob"));
        assert_ne!(other_key.id, first.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_view_is_pending_only_for_the_key_and_label_with_an_open_changeset() {
        let test = app_with(&[("pg", &[("a.md", "# A\n")])], Config::default()).await;
        store_key(&test, "writer").await;
        let writer = || {
            context(
                &test,
                principal_as("writer", Access::Write, &[], Some("pg")),
                None,
            )
        };
        let reader = context(&test, principal(&[], Some("pg")), None);
        let state = test.app.catalog("pg").unwrap();
        let before = writer().view(&state).await.unwrap();
        let edit = PendingEdit {
            catalog: "pg",
            path: "a.md",
            text: Some("# Pending\n"),
            at: Utc::now(),
        };

        pending_entry(&test, &writer(), &edit).await;
        let mine = writer().view(&state).await.unwrap();
        let other_label = writer().with_label("other").view(&state).await.unwrap();
        let read_key = reader.view(&state).await.unwrap();

        assert!(!before.pending());
        assert!(mine.pending());
        assert_eq!(mine.snapshot().get("a.md").unwrap().text(), "# Pending\n");
        assert!(!other_label.pending());
        assert!(!read_key.pending());
        assert_eq!(read_key.snapshot().get("a.md").unwrap().text(), "# A\n");
    }
}
