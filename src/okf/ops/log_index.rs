//! The bookkeeping ops `log` and `index` (`okf.py:752-820`) and
//! `ensure_index` (`:1471`). Each writes through the workspace and records
//! the [`ReplayOp`]s that redo its edit at publish (see [`mod@super::replay`]).
//!
//! Divergences from `okf.py`:
//! - a missing `log.md` is [`OkfError::NotFound`] (okf.py: `no log.md at <root>`);
//! - `ensure_index` upper-cases a folder name's first character where
//!   Python's `capitalize` title-cases it; they differ only for the few
//!   digraph letters such as `ǆ`;
//! - `ensure_index` needs no existing directory (`okf.py` expects `mv` to
//!   have created it): a workspace creates parents on write;
//! - `index` refuses a list or mapping `title` with "has no title", where
//!   `okf.py` crashes (`AttributeError` on `.replace`). Scalar titles such as
//!   `2026` or `true` are strings in both parsers, so they list alike.

use crate::error::OkfError;
use crate::okf::ops::replay::{
    folder_heading, insert_index_bullet_text, lists_target, log_text, missing_heading,
    new_index_text, ReplayOp,
};
use crate::okf::ops::{load_concept, refused, LogKind, OpContext, OpOutcome};
use crate::okf::paths::{basename, dirname, join};
use crate::okf::pystr::is_py_space;
use crate::okf::workspace::Workspace;
use crate::okf::yaml::Value;

/// The catalog's log, at the root.
const LOG: &str = "log.md";
/// The heading folder listings go under.
const SECTIONS: &str = "Sections";

/// Python's `" ".join(text.split())`: runs of whitespace become one space,
/// and leading and trailing whitespace goes.
fn collapse_whitespace(text: &str) -> String {
    text.split(is_py_space)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Port of `cmd_log`: adds `* **<kind>:** <text>` under today's (UTC)
/// heading in the root `log.md`.
///
/// # Arguments
/// * `workspace` - the catalog.
/// * `ctx` - its `now` gives the date.
/// * `kind` - the entry's kind.
/// * `text` - the entry; whitespace runs collapse to single spaces.
///
/// # Returns
/// The outcome, with a `Log` replay op for `log.md`; nothing is touched
/// when the bullet is already under today's heading.
///
/// # Errors
/// [`OkfError::NotFound`] without a `log.md`; the workspace's write error.
pub fn log(
    workspace: &dyn Workspace,
    ctx: &OpContext<'_>,
    kind: LogKind,
    text: &str,
) -> Result<OpOutcome, OkfError> {
    // Text mode with universal newlines, as `okf.py` reads `log.md`.
    let current = workspace.read_text(LOG)?;
    let date = ctx.now.format("%Y-%m-%d").to_string();
    let bullet = format!("* **{}:** {}", kind.as_str(), collapse_whitespace(text));
    let Some(updated) = log_text(&current, &date, &bullet) else {
        return Ok(OpOutcome::unchanged(format!(
            "{LOG}: already logged under {date}"
        )));
    };
    workspace.write_atomic(LOG, &updated)?;
    // A struct literal with `..default()` (not assignments after
    // `default()`, which clippy's `field_reassign_with_default` flags).
    let mut outcome = OpOutcome {
        message: format!("{LOG}: ## {date}: {bullet}"),
        ..OpOutcome::default()
    };
    outcome.record(LOG, ReplayOp::Log { date, bullet });
    Ok(outcome)
}

/// What [`list_concept`] lists: `index`'s arguments once resolved.
pub(crate) struct Listing<'a> {
    /// Catalog-relative path of the concept.
    pub(crate) concept: &'a str,
    /// The link text, before escaping.
    pub(crate) title: &'a str,
    /// The summary after ` - `; whitespace collapses.
    pub(crate) summary: &'a str,
    /// The heading to list under, if any.
    pub(crate) section: Option<&'a str>,
}

/// `cmd_index` after the concept is loaded: lists it in its folder's
/// `index.md` unless the index already links it, recording the replay op.
///
/// # Returns
/// `okf.py`'s message: `<folder>/index.md: <bullet>`, or
/// `<folder>/index.md: already lists <concept>`.
///
/// # Errors
/// [`OkfError::Refused`] without a folder `index.md`, or when `section`
/// names a missing heading; the workspace's read or write error.
pub(crate) fn list_concept(
    workspace: &dyn Workspace,
    listing: &Listing<'_>,
    outcome: &mut OpOutcome,
) -> Result<String, OkfError> {
    let folder = dirname(listing.concept);
    let index_path = join(folder, "index.md");
    if !workspace.exists(&index_path) {
        return Err(refused(format!(
            "no index.md in /{folder}; create it first"
        )));
    }
    let text = workspace.read_text(&index_path)?;
    if lists_target(workspace, &text, &index_path, listing.concept) {
        return Ok(format!("{index_path}: already lists {}", listing.concept));
    }
    let label = listing.title.replace('[', "\\[").replace(']', "\\]");
    let bullet = format!(
        "* [{label}]({}) - {}",
        basename(listing.concept),
        collapse_whitespace(listing.summary)
    );
    let updated = insert_index_bullet_text(&text, &bullet, listing.section, false)
        .ok_or_else(|| missing_heading(&index_path, listing.section.unwrap_or("")))?;
    workspace.write_atomic(&index_path, &updated)?;
    let message = format!("{index_path}: {bullet}");
    outcome.record(
        &index_path,
        ReplayOp::ListBullet {
            target: listing.concept.to_string(),
            bullet,
            section: listing.section.map(str::to_string),
            create_section: false,
        },
    );
    Ok(message)
}

/// Port of `cmd_index`: lists a concept in its folder's `index.md`.
///
/// # Arguments
/// * `workspace` - the catalog; `_ctx` is unused (no timestamp).
/// * `concept` - the concept reference.
/// * `summary` - the text after ` - `.
/// * `section` - the heading to list under; `None` for `# Concepts`.
/// * `title` - the link text; `None` (or empty) for the concept's `title`.
///
/// # Returns
/// The outcome, with a `ListBullet` replay op; nothing is touched when the
/// index already links the concept.
///
/// # Errors
/// As [`crate::okf::ops::stamp`] when loading the concept;
/// [`OkfError::Refused`] without a title, without a folder `index.md`, or
/// for a missing `section` heading.
pub fn index(
    workspace: &dyn Workspace,
    _ctx: &OpContext<'_>,
    concept: &str,
    summary: &str,
    section: Option<&str>,
    title: Option<&str>,
) -> Result<OpOutcome, OkfError> {
    let loaded = load_concept(workspace, concept)?;
    // `a.title or data.get("title")`: an empty string counts as absent. Every
    // YAML scalar is a `Value::Str`, so only a list or mapping is dropped here.
    let frontmatter_title = loaded.data.get("title").and_then(Value::as_str);
    let title = title
        .filter(|t| !t.is_empty())
        .or(frontmatter_title.filter(|t| !t.is_empty()))
        .ok_or_else(|| refused(format!("{} has no title; pass --title", loaded.path)))?;
    let listing = Listing {
        concept: &loaded.path,
        title,
        summary,
        section,
    };
    let mut outcome = OpOutcome::default();
    let message = list_concept(workspace, &listing, &mut outcome)?;
    outcome.message = message;
    Ok(outcome)
}

/// Port of `ensure_index`: makes sure `folder/index.md` exists and is
/// listed in its parent's index, creating missing ancestor indexes too.
///
/// As in `okf.py`, the folder's own `index.md` is written first; then the
/// parent is ensured (recursively), and only after that is the folder listed
/// in it. So the notes come out outermost-first.
///
/// A new index is `# <Folder name>` and a TODO line; its parent gains
/// `* [<Folder name>](<name>/) - TODO: describe this folder` under
/// `# Sections` (created when missing), with an absolute `/<folder>/` href
/// when the parent's bullets are absolute.
///
/// # Arguments
/// * `workspace` - the catalog.
/// * `folder` - catalog-relative folder; `""` (the root) does nothing.
/// * `outcome` - gains each file written, its replay ops (`CreateIndex`,
///   then the parent's `ListBullet`) and one note per created index.
///
/// # Errors
/// The workspace's read or write error (a missing root `index.md` is
/// [`OkfError::NotFound`]). Files written before the error stay written,
/// so run it on a scratch copy (as the write path does) and drop the copy
/// on error.
pub fn ensure_index(
    workspace: &dyn Workspace,
    folder: &str,
    outcome: &mut OpOutcome,
) -> Result<(), OkfError> {
    if folder.is_empty() {
        return Ok(());
    }
    let index_path = join(folder, "index.md");
    if workspace.exists(&index_path) {
        return Ok(());
    }
    workspace.write_atomic(&index_path, &new_index_text(folder))?;
    outcome.record(
        &index_path,
        ReplayOp::CreateIndex {
            folder: folder.to_string(),
        },
    );
    let parent = dirname(folder);
    ensure_index(workspace, parent, outcome)?;
    let parent_index = join(parent, "index.md");
    let parent_text = workspace.read_text(&parent_index)?;
    let absolute = parent_text
        .split('\n')
        .any(|line| line.starts_with("* [") && line.contains("](/"));
    let href = if absolute {
        format!("/{folder}/")
    } else {
        format!("{}/", basename(folder))
    };
    let bullet = format!(
        "* [{}]({href}) - TODO: describe this folder",
        folder_heading(folder)
    );
    let updated = insert_index_bullet_text(&parent_text, &bullet, Some(SECTIONS), true)
        .expect("create_section always finds a place for the bullet");
    workspace.write_atomic(&parent_index, &updated)?;
    outcome.record(
        &parent_index,
        ReplayOp::ListBullet {
            target: index_path,
            bullet,
            section: Some(SECTIONS.to_string()),
            create_section: true,
        },
    );
    outcome.notes.push(format!(
        "created /{folder}/index.md; replace its TODO lines (and the parent's listing)"
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;
    use chrono::{TimeZone, Utc};

    const INFRA_INDEX: &str =
        "# Infra\n\n# Concepts\n\n* [Time server](time-server.md) - GPS clock\n";

    fn ctx() -> OpContext<'static> {
        OpContext {
            now: Utc.with_ymd_and_hms(2026, 10, 27, 12, 0, 0).unwrap(),
            by: "tester",
        }
    }

    fn refusal(error: OkfError) -> String {
        match error {
            OkfError::Refused { reason } => reason,
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    fn infra() -> MemWorkspace {
        MemWorkspace::from_files([
            ("infra/index.md", INFRA_INDEX),
            (
                "infra/ntp.md",
                "---\ntype: Service\ntitle: NTP [pool]\n---\n",
            ),
            ("infra/untitled.md", "---\ntype: Note\n---\n"),
            ("people/x.md", "---\ntype: Person\ntitle: X\n---\n"),
        ])
    }

    #[test]
    fn log_writes_the_entry_and_records_its_replay() {
        let ws = MemWorkspace::from_files([("log.md", "# Log\n")]);

        let outcome = log(&ws, &ctx(), LogKind::Creation, "  Added   the\n pool. ").unwrap();

        assert_eq!(
            ws.read_to_string("log.md").unwrap(),
            "# Log\n\n## 2026-10-27\n\n* **Creation:** Added the pool.\n"
        );
        assert_eq!(outcome.touched, ["log.md"]);
        assert_eq!(
            outcome.replay["log.md"],
            [ReplayOp::Log {
                date: "2026-10-27".to_string(),
                bullet: "* **Creation:** Added the pool.".to_string(),
            }]
        );
        assert_eq!(
            outcome.message,
            "log.md: ## 2026-10-27: * **Creation:** Added the pool."
        );
    }

    #[test]
    fn log_reports_a_duplicate_and_needs_log_md() {
        let ws = MemWorkspace::from_files([("log.md", "# Log\n")]);
        log(&ws, &ctx(), LogKind::Update, "Once.").unwrap();

        let again = log(&ws, &ctx(), LogKind::Update, "Once.").unwrap();
        let missing = log(&MemWorkspace::default(), &ctx(), LogKind::Update, "x").unwrap_err();

        assert!(again.touched.is_empty());
        assert_eq!(again.message, "log.md: already logged under 2026-10-27");
        assert!(matches!(missing, OkfError::NotFound { .. }));
    }

    #[test]
    fn index_lists_a_concept_with_its_title_escaped() {
        let ws = infra();

        let outcome = index(&ws, &ctx(), "infra/ntp", "  the   pool ", None, None).unwrap();

        let bullet = "* [NTP \\[pool\\]](ntp.md) - the pool";
        assert_eq!(
            ws.read_to_string("infra/index.md").unwrap(),
            format!("{INFRA_INDEX}{bullet}\n")
        );
        assert_eq!(outcome.message, format!("infra/index.md: {bullet}"));
        assert_eq!(
            outcome.replay["infra/index.md"],
            [ReplayOp::ListBullet {
                target: "infra/ntp.md".to_string(),
                bullet: bullet.to_string(),
                section: None,
                create_section: false,
            }]
        );
    }

    #[test]
    fn index_of_a_listed_concept_changes_nothing() {
        let ws = infra();
        index(&ws, &ctx(), "infra/ntp", "pool", None, None).unwrap();

        let again = index(&ws, &ctx(), "infra/ntp.md", "other", None, Some("T")).unwrap();

        assert!(again.touched.is_empty());
        assert_eq!(again.message, "infra/index.md: already lists infra/ntp.md");
    }

    #[test]
    fn index_refusals_use_okf_py_wording() {
        let ws = infra();

        let untitled = index(&ws, &ctx(), "infra/untitled", "s", None, None).unwrap_err();
        let no_index = index(&ws, &ctx(), "people/x", "s", None, None).unwrap_err();
        let no_heading = index(
            &ws,
            &ctx(),
            "infra/untitled",
            "s",
            Some("Nowhere"),
            Some("T"),
        )
        .unwrap_err();

        assert_eq!(
            refusal(untitled),
            "infra/untitled.md has no title; pass --title"
        );
        assert_eq!(refusal(no_index), "no index.md in /people; create it first");
        assert_eq!(
            refusal(no_heading),
            "no heading 'Nowhere' in infra/index.md"
        );
        assert_eq!(ws.read_to_string("infra/index.md").unwrap(), INFRA_INDEX);
    }

    #[test]
    fn index_lists_a_scalar_title_and_refuses_a_list_title() {
        let ws = MemWorkspace::from_files([
            ("infra/index.md", INFRA_INDEX),
            ("infra/year.md", "---\ntype: Note\ntitle: 2026\n---\n"),
            ("infra/listed.md", "---\ntype: Note\ntitle: [a, b]\n---\n"),
        ]);

        let year = index(&ws, &ctx(), "infra/year", "y", None, None).unwrap();
        let listed = index(&ws, &ctx(), "infra/listed", "l", None, None).unwrap_err();

        // okf.py writes the same bullet for `title: 2026`, and crashes on the list.
        assert_eq!(year.message, "infra/index.md: * [2026](year.md) - y");
        assert_eq!(
            refusal(listed),
            "infra/listed.md has no title; pass --title"
        );
    }

    #[test]
    fn ensure_index_follows_the_parents_link_style() {
        let ws = MemWorkspace::from_files([
            ("index.md", "# Root\n\n* [Infra](infra/) - systems\n"),
            (
                "analysis/index.md",
                "# Analysis\n\n# Sections\n\n* [Sub](/analysis/sub/) - a folder\n",
            ),
        ]);
        let mut absolute = OpOutcome::default();
        let mut nested = OpOutcome::default();

        ensure_index(&ws, "analysis/new-stuff", &mut absolute).unwrap();
        ensure_index(&ws, "ops/deep", &mut nested).unwrap();

        assert_eq!(
            absolute.touched,
            ["analysis/new-stuff/index.md", "analysis/index.md"]
        );
        assert_eq!(
            ws.read_to_string("analysis/new-stuff/index.md").unwrap(),
            "# New stuff\n\nTODO: say what this folder holds.\n"
        );
        assert_eq!(
            ws.read_to_string("analysis/index.md").unwrap(),
            concat!(
                "# Analysis\n\n# Sections\n\n* [Sub](/analysis/sub/) - a folder\n",
                "* [New stuff](/analysis/new-stuff/) - TODO: describe this folder\n",
            )
        );
        assert_eq!(
            absolute.replay["analysis/index.md"],
            [ReplayOp::ListBullet {
                target: "analysis/new-stuff/index.md".to_string(),
                bullet: "* [New stuff](/analysis/new-stuff/) - TODO: describe this folder"
                    .to_string(),
                section: Some("Sections".to_string()),
                create_section: true,
            }]
        );
        assert_eq!(
            nested.touched,
            ["ops/deep/index.md", "ops/index.md", "index.md"]
        );
        assert_eq!(
            ws.read_to_string("index.md").unwrap(),
            concat!(
                "# Root\n\n* [Infra](infra/) - systems\n\n# Sections\n\n",
                "* [Ops](ops/) - TODO: describe this folder\n",
            )
        );
        assert_eq!(
            nested.notes,
            [
                "created /ops/index.md; replace its TODO lines (and the parent's listing)",
                "created /ops/deep/index.md; replace its TODO lines (and the parent's listing)",
            ]
        );
    }

    #[test]
    fn ensure_index_of_an_existing_folder_or_the_root_does_nothing() {
        let ws = MemWorkspace::from_files([("index.md", "# Root\n"), ("ops/index.md", "# Ops\n")]);
        let mut outcome = OpOutcome::default();

        ensure_index(&ws, "ops", &mut outcome).unwrap();
        ensure_index(&ws, "", &mut outcome).unwrap();

        assert_eq!(outcome, OpOutcome::default());
    }
}
