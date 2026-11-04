//! What one tool call acts as and on: the principal, the connection's
//! catalogs (and, until M8 Task 5, its changeset label, which nothing looks
//! up any more), one view of the catalog map, and the changeset a handle
//! names ([`Pinned`]).

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
use crate::store::changesets::{self, ChangesetRow, ChangesetState};
use crate::store::keys::Access;

/// Is this [`ToolContext::view`] error a changeset's overlay failing to
/// apply, rather than the database failing?
///
/// Only [`crate::overlay::build_overlay`] reports one, always as an
/// [`OkfError::InvalidPath`] naming the changeset: a clash with the
/// published files, or a pending file the snapshot cannot read. The match is
/// positive, so any other error a later change lets `view` return is not
/// mistaken for one. A read across several catalogs leaves such a catalog
/// out and carries on; any other error (a database error, `Busy`) fails the
/// call, so the agent is told to retry instead of seeing fewer results.
///
/// # Arguments
/// * `error` - the error `view` returned.
///
/// # Returns
/// `true` for an overlay that does not apply.
pub fn is_broken_overlay(error: &OkfError) -> bool {
    matches!(error, OkfError::InvalidPath { .. })
}

/// A changeset a call acts on, and its catalog (M8 Decision 5: the catalog
/// comes from the changeset).
#[derive(Debug, Clone)]
pub struct Pinned {
    /// The changeset's catalog.
    pub state: Arc<CatalogState>,
    /// The changeset, as read when the call resolved it (not locked).
    pub row: ChangesetRow,
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

    /// The call's changeset, if it named one: what a read tool passes to
    /// [`ToolContext::view`] (M8 Decision 6). Reads need only `read` access,
    /// so the key that owns the changeset, or an admin, may read through it.
    ///
    /// # Arguments
    /// * `handle` - the call's optional `changeset` argument.
    ///
    /// # Returns
    /// The pinned changeset, or `None` without a handle.
    ///
    /// # Errors
    /// As [`ToolContext::changeset`].
    pub async fn pin(&self, handle: Option<&str>) -> Result<Option<Pinned>, OkfError> {
        match handle {
            None => Ok(None),
            // `.map(Some)` wraps the `Ok` value: `Ok(pinned)` becomes `Ok(Some(pinned))`.
            Some(handle) => self.changeset(handle, Access::Read).await.map(Some),
        }
    }

    /// What this call reads of `state`: the pinned changeset's overlay if
    /// it is in this catalog, else the published state (M8 Decision 6).
    /// Without a pinned changeset a read is always published, whatever the
    /// key's open changesets.
    ///
    /// The pinned row was read unlocked when the handle resolved. The overlay
    /// cache matches that row's `version` and the catalog's generation, and
    /// every write, publish and discard forgets the cached overlay, so a write
    /// that commits meanwhile is seen by the next call, never cached stale.
    ///
    /// # Arguments
    /// * `state` - a catalog this call selected.
    /// * `pinned` - the call's changeset, from [`ToolContext::pin`].
    ///
    /// # Returns
    /// The view.
    ///
    /// # Errors
    /// An overlay build error ([`crate::overlay::build_overlay`]);
    /// [`OkfError::Database`].
    pub async fn view(
        &self,
        state: &CatalogState,
        pinned: Option<&Pinned>,
    ) -> Result<View, OkfError> {
        // `filter` keeps the pinned changeset only if it is in this catalog.
        match pinned.filter(|pinned| pinned.state.id() == state.id()) {
            None => Ok(View::Published(state.current())),
            Some(pinned) => {
                let overlay = self
                    .app
                    .overlays()
                    .get(self.app.pool(), state, &pinned.row)
                    .await?;
                Ok(View::Pending(overlay))
            }
        }
    }

    /// The one catalog a report runs on, given its changeset: `catalog`, else
    /// the pinned changeset's catalog, else the first selected. A `pg`
    /// handle on a `work,pg` connection reports on `pg`.
    ///
    /// The pinned changeset's catalog stands in for a `catalog` argument, and
    /// an argument wins over the connection's catalogs (design "Catalog
    /// selection"), so it needs only the key's reach, not the connection's
    /// selection: a `pg` handle on a `work`-only connection reports on `pg`
    /// too. [`ToolContext::changeset`] has already checked that the key
    /// reaches it, so `one` cannot refuse it.
    ///
    /// # Arguments
    /// * `catalog` - the call's `catalog` argument.
    /// * `pinned` - the call's changeset, from [`ToolContext::pin`].
    ///
    /// # Returns
    /// That catalog's state.
    ///
    /// # Errors
    /// As [`ToolContext::one`] for an explicit `catalog` (or, without a
    /// changeset, the connection's selection); as [`require_selected`] for an
    /// explicit `catalog` that is not the changeset's.
    pub fn report_catalog(
        &self,
        catalog: Option<&str>,
        pinned: Option<&Pinned>,
    ) -> Result<Arc<CatalogState>, OkfError> {
        // `or_else` runs the closure only when `catalog` is `None`.
        let name = catalog.or_else(|| pinned.map(|pinned| pinned.state.name()));
        let state = self.one(name)?;
        require_selected(pinned, std::slice::from_ref(&state))?;
        Ok(state)
    }

    /// Resolves a changeset handle for this call (M8 Decisions 1 and 3).
    ///
    /// # Arguments
    /// * `handle` - the call's `changeset` argument.
    /// * `access` - what the call needs on the changeset's catalog.
    ///
    /// # Returns
    /// The open changeset and its catalog. The row is read without a lock:
    /// a write locks it again (`changesets::lock`) before trusting its state.
    ///
    /// # Errors
    /// - [`OkfError::UnknownChangeset`] for a handle that does not exist, is
    ///   in a catalog this key does not reach (or that is not loaded), or
    ///   belongs to another key while this one is not `admin`;
    /// - [`OkfError::Forbidden`] below `access`;
    /// - [`OkfError::Refused`] for a changeset that was published or
    ///   discarded (a discarded one with a hint that an empty changeset is
    ///   discarded after 24 hours);
    /// - [`OkfError::Database`].
    pub async fn changeset(&self, handle: &str, access: Access) -> Result<Pinned, OkfError> {
        // A closure, so each refusal below builds its own error value.
        let unknown = || OkfError::UnknownChangeset {
            changeset: handle.to_string(),
        };
        let row = changesets::by_handle(self.app.pool(), handle)
            .await?
            .ok_or_else(unknown)?;
        // The map is keyed by name; a handful of catalogs, so a scan is fine.
        let state = self
            .catalogs
            .values()
            .find(|state| state.id() == row.catalog_id)
            .map(Arc::clone)
            .ok_or_else(unknown)?;
        // Ownership is checked before `require(access, ..)`, on purpose: a key
        // that neither owns the changeset nor is `admin` gets
        // `UnknownChangeset`, never `Forbidden`, so it cannot tell a real
        // handle from a made-up one (Decision 3). So a `read` key, which owns
        // none, never resolves one, and `access` only matters for owners and
        // admins. Don't reorder these checks.
        let own = row.key_id.as_deref() == Some(self.principal.key_id.as_str());
        let admin = self.principal.access == Access::Admin;
        if !self.principal.reaches(state.name()) || !(own || admin) {
            return Err(unknown());
        }
        self.principal.require(access, state.name())?;
        // Only a discarded changeset gets the 24-hour hint: the prune is one
        // way to be discarded, and a published one was never pruned.
        let hint = match row.state {
            ChangesetState::Open => return Ok(Pinned { state, row }),
            ChangesetState::Published => "",
            ChangesetState::Discarded => " (a changeset with no edits is discarded after 24 hours)",
        };
        Err(OkfError::Refused {
            reason: format!(
                "changeset {handle} is {}; open a new one with open_changeset{hint}",
                row.state.as_str()
            ),
        })
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

/// Refuses a pinned changeset whose catalog the call does not read (M8
/// Decision 6), so a handle never silently reads as published.
///
/// # Arguments
/// * `pinned` - the call's changeset, if any.
/// * `states` - the catalogs the call reads.
///
/// # Errors
/// [`OkfError::Refused`] naming the changeset's catalog.
pub fn require_selected(
    pinned: Option<&Pinned>,
    states: &[Arc<CatalogState>],
) -> Result<(), OkfError> {
    // `let ... else`: without a pinned changeset there is nothing to check.
    let Some(pinned) = pinned else {
        return Ok(());
    };
    if states.iter().any(|state| state.id() == pinned.state.id()) {
        return Ok(());
    }
    Err(OkfError::Refused {
        reason: format!(
            "changeset {} is in catalog {:?}, which this call does not read",
            pinned.row.handle,
            pinned.state.name()
        ),
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::{require_selected, ToolContext};
    use crate::config::Config;
    use crate::mcp::changesets::{open_changeset, OpenParams};
    use crate::mcp::testing::{
        app_with, context, open, pending_entry, principal, principal_as, store_key, PendingEdit,
    };
    use crate::selection::{CHANGESET_HEADER, DEFAULT_LABEL};
    use crate::store::changesets::{self, ChangesetState};
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

    /// `pg`, the keys `writer`, `other` and `root`, and a changeset the
    /// writer opened in `pg` (through `open_changeset`, as a client would).
    async fn opened_by_writer() -> (crate::mcp::testing::TestApp, String) {
        let test = app_with(&[("pg", &[])], Config::default()).await;
        for key in ["writer", "other", "root"] {
            store_key(&test, key).await;
        }
        let writer = as_key(&test, "writer", Access::Write);
        let opened = open_changeset(&writer, OpenParams { catalog: None })
            .await
            .unwrap();
        (test, opened.changeset)
    }

    fn as_key(test: &crate::mcp::testing::TestApp, key: &str, access: Access) -> ToolContext {
        context(test, principal_as(key, access, &[], Some("pg")), None)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_handle_resolves_for_its_key_and_an_admin_only() {
        let (test, handle) = opened_by_writer().await;

        let mine = as_key(&test, "writer", Access::Write)
            .changeset(&handle, Access::Write)
            .await
            .unwrap();
        let admin = as_key(&test, "root", Access::Admin)
            .changeset(&handle, Access::Write)
            .await
            .unwrap();
        let other = as_key(&test, "other", Access::Write)
            .changeset(&handle, Access::Write)
            .await
            .unwrap_err();
        let made_up = as_key(&test, "writer", Access::Write)
            .changeset("made-up", Access::Write)
            .await
            .unwrap_err();

        assert_eq!(
            (mine.state.name(), mine.row.handle.as_str()),
            ("pg", handle.as_str())
        );
        assert_eq!(admin.row.id, mine.row.id);
        assert_eq!(other.kind(), "UnknownChangeset");
        assert_eq!(made_up.kind(), "UnknownChangeset");
        assert_eq!(
            other.to_string().replace(&handle, "made-up"),
            made_up.to_string()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_closed_changeset_is_refused_by_name() {
        let (test, discarded) = opened_by_writer().await;
        let writer = as_key(&test, "writer", Access::Write);
        let published = open_changeset(&writer, OpenParams { catalog: None })
            .await
            .unwrap()
            .changeset;
        let mut tx = test.app.pool().begin().await.unwrap();
        for (handle, closed) in [
            (&discarded, ChangesetState::Discarded),
            (&published, ChangesetState::Published),
        ] {
            let row = writer.changeset(handle, Access::Write).await.unwrap().row;
            changesets::set_state(&mut tx, row.id, closed, Utc::now())
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();

        let refused = writer
            .changeset(&discarded, Access::Write)
            .await
            .unwrap_err();
        let refused_published = writer
            .changeset(&published, Access::Write)
            .await
            .unwrap_err();

        assert_eq!(refused.kind(), "Refused");
        let message = refused.to_string();
        assert!(message.contains(&discarded), "{message}");
        assert!(message.contains("discarded"), "{message}");
        assert!(message.contains("24 hours"), "{message}");
        // Only the prune discards after 24 hours; a published one gets no hint.
        assert_eq!(refused_published.kind(), "Refused");
        assert_eq!(
            refused_published.to_string(),
            format!("changeset {published} is published; open a new one with open_changeset")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_read_key_resolves_nothing() {
        let (test, handle) = opened_by_writer().await;
        let reader = context(&test, principal(&[], Some("pg")), None);

        let refused = reader.changeset(&handle, Access::Read).await.unwrap_err();

        assert_eq!(refused.kind(), "UnknownChangeset");
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
    async fn a_read_without_a_changeset_is_published_even_for_a_writer_with_drafts() {
        let test = app_with(&[("pg", &[("a.md", "# A\n")])], Config::default()).await;
        store_key(&test, "writer").await;
        let writer = as_key(&test, "writer", Access::Write);
        let handle = open(&writer).await;
        let edit = PendingEdit {
            path: "a.md",
            text: Some("# Pending\n"),
            at: Utc::now(),
        };
        pending_entry(&test, &handle, &edit).await;
        let state = test.app.catalog("pg").unwrap();

        let pinned = writer.pin(Some(&handle)).await.unwrap();
        let unpinned = writer.pin(None).await.unwrap();
        let published = writer.view(&state, None).await.unwrap();
        let drafts = writer.view(&state, pinned.as_ref()).await.unwrap();

        assert!(unpinned.is_none());
        assert!(!published.pending());
        assert_eq!(published.snapshot().get("a.md").unwrap().text(), "# A\n");
        assert!(drafts.pending());
        assert_eq!(drafts.snapshot().get("a.md").unwrap().text(), "# Pending\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_pinned_changeset_applies_only_to_its_own_catalog() {
        let test = app_with(
            &[("pg", &[("a.md", "# A\n")]), ("work", &[("a.md", "# W\n")])],
            Config::default(),
        )
        .await;
        store_key(&test, "writer").await;
        let writer = as_key(&test, "writer", Access::Write);
        let handle = open(&writer).await;
        let edit = PendingEdit {
            path: "a.md",
            text: Some("# Pending\n"),
            at: Utc::now(),
        };
        pending_entry(&test, &handle, &edit).await;
        let pinned = writer.pin(Some(&handle)).await.unwrap();
        let pg = test.app.catalog("pg").unwrap();
        let work = test.app.catalog("work").unwrap();

        let in_work = writer.view(&work, pinned.as_ref()).await.unwrap();
        let in_pg = writer.view(&pg, pinned.as_ref()).await.unwrap();
        let outside = require_selected(pinned.as_ref(), std::slice::from_ref(&work)).unwrap_err();
        let inside = require_selected(pinned.as_ref(), &[work, pg]);

        assert!(!in_work.pending());
        assert_eq!(in_work.snapshot().get("a.md").unwrap().text(), "# W\n");
        assert!(in_pg.pending());
        assert_eq!(outside.kind(), "Refused");
        let message = outside.to_string();
        assert!(
            message.contains(&handle) && message.contains("\"pg\""),
            "{message}"
        );
        assert!(inside.is_ok());
        assert!(require_selected(None, &[]).is_ok());
    }
}
