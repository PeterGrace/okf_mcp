//! `delete_concept` (design "Write tools"; `okf.py` has no counterpart).
//! It removes a concept and its folder listing, and refuses while any other
//! document links to it ([`OkfError::StillLinked`], listing those links).
//!
//! What counts as a link (plan Decision 4) is what publish's link check
//! reads: every markdown file in the walk except the root `AGENTS.md`,
//! `CLAUDE.md` and `README.md`, so `index.md` and `log.md` files count. The
//! folder's own listing (the first bullet of its `index.md` linking the
//! concept, which this op removes) and the concept's links to itself do not.
//!
//! The listing goes through a [`ReplayOp::Unlist`] (any bullet, since it is
//! deleted either way), so publish replays it onto newer text. The concept
//! is removed with [`MemWorkspace::remove`], and `write::apply` stores a
//! deletion. An emptied folder keeps its `index.md`; a note says so.

use crate::error::{LinkSource, OkfError};
use crate::okf::markdown::{find_links, resolve_link};
use crate::okf::ops::replay::{listing_line, unlist_text, ReplayOp};
use crate::okf::ops::OpOutcome;
use crate::okf::paths::{dirname, join, outside_link_graph, resolve_concept};
use crate::okf::rules::check_path;
use crate::okf::workspace::{MemWorkspace, Workspace};

/// Deletes a concept and its listing in its folder's `index.md`.
///
/// # Arguments
/// * `workspace` - the catalog (the write path's scratch copy of an overlay).
/// * `reference` - the concept, `/dir/x.md`, `dir/x` or `/dir/x`.
///
/// # Returns
/// The outcome: the concept (removed) and, if it was listed, the folder
/// index with its `Unlist` op; the message `deleted /<path>`; a note when
/// the folder is left holding only its `index.md`.
///
/// # Errors
/// - As [`resolve_concept`] ([`OkfError::NotFound`]; [`OkfError::InvalidPath`]
///   for `index.md`, `log.md` and the root readmes).
/// - [`OkfError::InvalidPath`] for a concept in machinery ([`check_path`]).
/// - [`OkfError::StillLinked`] while another document links to it.
/// - The workspace's read or write error. Every walked markdown file is read,
///   so one that is not UTF-8, anywhere in the catalog, is [`OkfError::Io`],
///   as it is for `Snapshot::build` (and so for publish). A stored catalog
///   holds none: import refuses such a file and the write tools write only
///   UTF-8.
pub fn delete_concept(workspace: &MemWorkspace, reference: &str) -> Result<OpOutcome, OkfError> {
    let path = check_path(&resolve_concept(workspace, reference)?)?;
    let folder = dirname(&path);
    let index = join(folder, "index.md");
    let index_text = if workspace.is_file(&index) {
        Some(workspace.read_text(&index)?)
    } else {
        None
    };
    // `as_deref` views `Option<String>` as `Option<&str>`; `and_then` chains
    // the lookup, like `text and listing_line(...)` in Python.
    let listing = index_text
        .as_deref()
        .and_then(|text| listing_line(workspace, text, &index, &path));
    let links = links_into(workspace, &path, &index, listing)?;
    if !links.is_empty() {
        return Err(OkfError::StillLinked { path, links });
    }
    let unlisted = index_text
        .as_deref()
        .and_then(|text| unlist_text(workspace, text, &index, &path));
    workspace.remove(&path)?;
    let mut outcome = OpOutcome::default();
    outcome.touch(&path);
    if let Some((rest, _)) = unlisted {
        workspace.write_atomic(&index, &rest)?;
        let op = ReplayOp::Unlist {
            target: path.clone(),
            bullet: None,
        };
        outcome.record(&index, op);
    }
    if !folder.is_empty() && only_the_index_is_left(workspace, folder, &index)? {
        outcome.notes.push(format!(
            "note: /{folder}/ now holds only its index.md, which stays; delete_concept never \
             deletes an index"
        ));
    }
    outcome.message = format!("deleted /{path}");
    Ok(outcome)
}

/// Every link into `path` from another document, except the folder
/// listing's line (`listing`, a 0-based line of `index`).
///
/// # Errors
/// The workspace's read error.
fn links_into(
    workspace: &dyn Workspace,
    path: &str,
    index: &str,
    listing: Option<usize>,
) -> Result<Vec<LinkSource>, OkfError> {
    let mut links = Vec::new();
    for source in workspace.list_markdown()? {
        if outside_link_graph(&source) || source == path {
            continue;
        }
        // Universal newlines, as the snapshot (and so lint) numbers lines.
        let text = workspace.read_text(&source)?;
        for (number, line) in text.split('\n').enumerate() {
            if source == index && listing == Some(number) {
                continue;
            }
            // In-page links (`#x`) resolve to `None`, so they never count.
            let hits = find_links(line)
                .iter()
                .filter(|link| {
                    resolve_link(workspace, &link.target, &source)
                        .is_some_and(|resolved| resolved.path == path)
                })
                .count();
            // `repeat_with(..).take(n)`: one entry per link on the line.
            links.extend(
                std::iter::repeat_with(|| LinkSource {
                    source: source.clone(),
                    line: number + 1,
                })
                .take(hits),
            );
        }
    }
    Ok(links)
}

/// Does `folder` hold its `index.md` and nothing else?
///
/// # Errors
/// The workspace's listing error (none for a `MemWorkspace`).
fn only_the_index_is_left(
    workspace: &dyn Workspace,
    folder: &str,
    index: &str,
) -> Result<bool, OkfError> {
    let prefix = format!("{folder}/");
    // The `is_file` check first: a folder whose last file was the concept,
    // and which never had an index, is simply gone.
    Ok(workspace.is_file(index)
        && workspace
            .list_files()?
            .iter()
            .filter(|file| file.starts_with(&prefix))
            .all(|file| file == index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::ops::replay::replay;

    const INFRA_INDEX: &str = concat!(
        "# Infra\n\n# Concepts\n\n* [X](x.md) - x\n* [Y](y.md) - y\n",
        "* [X again](/infra/x.md) - listed twice\n* [W](w.md) - w\n",
    );
    const FILES: [(&str, &str); 11] = [
        (
            "index.md",
            "# Root\n\n# Sections\n\n* [Infra](infra/) - systems\n\nSee the [hub](/hub.md).\n",
        ),
        ("AGENTS.md", "Read [x](infra/x.md).\n"),
        (
            "hub.md",
            "---\ntype: Note\ntitle: Hub\n---\nSee [x](/infra/x.md#ports) and [y](infra/y.md).\n",
        ),
        (
            "log.md",
            "# Log\n\n## 2026-11-01\n\n* **Creation:** Added [x](/infra/x.md).\n",
        ),
        ("infra/index.md", INFRA_INDEX),
        (
            "infra/x.md",
            "---\ntype: Note\ntitle: X\n---\n# X\n\nSee [itself](#x) and [self](/infra/x.md).\n",
        ),
        ("infra/y.md", "---\ntype: Note\ntitle: Y\n---\n# Y\n"),
        ("infra/w.md", "---\ntype: Note\ntitle: W\n---\n# W\n"),
        ("infra/z.md", "---\ntype: Note\ntitle: Z\n---\n# Z\n"),
        (
            "solo/index.md",
            "# Solo\n\n* [Only](only.md) - the only one\n",
        ),
        (
            "solo/only.md",
            "---\ntype: Note\ntitle: Only\n---\n# Only\n",
        ),
    ];

    fn link(source: &str, line: usize) -> LinkSource {
        LinkSource {
            source: source.to_string(),
            line,
        }
    }

    #[test]
    fn a_linked_concept_is_refused_with_every_link_but_its_listing() {
        let workspace = MemWorkspace::from_files(FILES);

        let error = delete_concept(&workspace, "/infra/x").unwrap_err();

        // Not counted: the root AGENTS.md, the first bullet in infra/index.md
        // (the listing delete removes), x.md's links to itself.
        assert!(
            matches!(
                error,
                OkfError::StillLinked { ref path, ref links } if path == "infra/x.md" && links == &[
                    link("hub.md", 5),
                    link("log.md", 5),
                    link("infra/index.md", 7),
                ]
            ),
            "{error:?}"
        );
        assert!(workspace.is_file("infra/x.md"));
    }

    #[test]
    fn a_relative_link_counts_too() {
        let workspace = MemWorkspace::from_files(FILES);

        let error = delete_concept(&workspace, "infra/y.md").unwrap_err();

        assert!(matches!(
            error,
            OkfError::StillLinked { ref links, .. } if links == &[link("hub.md", 5)]
        ));
    }

    #[test]
    fn an_unlinked_concept_goes_with_its_listing() {
        let workspace = MemWorkspace::from_files(FILES);

        let outcome = delete_concept(&workspace, "infra/w.md").unwrap();

        assert_eq!(outcome.message, "deleted /infra/w.md");
        assert_eq!(outcome.touched, ["infra/w.md", "infra/index.md"]);
        assert!(outcome.notes.is_empty());
        assert!(!workspace.is_file("infra/w.md"));
        let listed = workspace.read_to_string("infra/index.md").unwrap();
        assert_eq!(listed, INFRA_INDEX.replace("* [W](w.md) - w\n", ""));
        let ops = &outcome.replay["infra/index.md"];
        assert_eq!(
            ops,
            &[ReplayOp::Unlist {
                target: "infra/w.md".to_string(),
                bullet: None
            }]
        );
        assert_eq!(
            replay("infra/index.md", Some(INFRA_INDEX), ops).unwrap(),
            listed
        );
    }

    #[test]
    fn an_unlisted_concept_touches_only_itself() {
        let workspace = MemWorkspace::from_files(FILES);

        let outcome = delete_concept(&workspace, "infra/z.md").unwrap();

        assert_eq!(outcome.touched, ["infra/z.md"]);
        assert!(outcome.replay.is_empty());
        assert_eq!(
            workspace.read_to_string("infra/index.md").unwrap(),
            INFRA_INDEX
        );
    }

    #[test]
    fn a_root_level_concept_is_unlisted_from_the_root_index_with_no_note() {
        let workspace = MemWorkspace::from_files([
            (
                "index.md",
                "# Root\n\n* [Top](top.md) - t\n* [Other](other.md) - o\n",
            ),
            ("top.md", "---\ntype: Note\ntitle: Top\n---\n# Top\n"),
            ("other.md", "---\ntype: Note\ntitle: Other\n---\n# Other\n"),
        ]);

        let outcome = delete_concept(&workspace, "top").unwrap();

        // `folder` is "", so the listing is the root `index.md`, and the
        // emptied-folder note never applies to the root.
        assert_eq!(outcome.touched, ["top.md", "index.md"]);
        assert!(outcome.notes.is_empty());
        assert_eq!(
            workspace.read_to_string("index.md").unwrap(),
            "# Root\n\n* [Other](other.md) - o\n"
        );
    }

    #[test]
    fn a_crlf_index_is_unlisted_with_newline_endings_like_its_replay() {
        let crlf = "# Infra\r\n\r\n* [W](w.md) - w\r\n* [Y](y.md) - y\r\n";
        let workspace = MemWorkspace::from_files([
            ("infra/index.md", crlf),
            ("infra/w.md", "---\ntype: Note\ntitle: W\n---\n# W\n"),
            ("infra/y.md", "---\ntype: Note\ntitle: Y\n---\n# Y\n"),
        ]);

        let outcome = delete_concept(&workspace, "infra/w.md").unwrap();

        // `Unlist` works on universal-newline text (plan Task 2), so the
        // op and its replay both write `\n` endings.
        let listed = workspace.read_to_string("infra/index.md").unwrap();
        assert_eq!(listed, "# Infra\n\n* [Y](y.md) - y\n");
        let ops = &outcome.replay["infra/index.md"];
        assert_eq!(replay("infra/index.md", Some(crlf), ops).unwrap(), listed);
    }

    #[test]
    fn an_emptied_folder_keeps_its_index_and_says_so() {
        let workspace = MemWorkspace::from_files(FILES);

        let outcome = delete_concept(&workspace, "solo/only.md").unwrap();

        assert_eq!(
            outcome.notes,
            [
                "note: /solo/ now holds only its index.md, which stays; delete_concept never \
              deletes an index"
            ]
        );
        // Only the bullet's line goes, as `unlist_text` (and its replay) take it.
        assert_eq!(
            workspace.read_to_string("solo/index.md").unwrap(),
            "# Solo\n\n"
        );
    }

    #[test]
    fn non_concepts_machinery_and_missing_paths_are_refused() {
        let mut files = FILES.to_vec();
        files.push(("install/setup.md", "---\ntype: Note\n---\n"));
        let workspace = MemWorkspace::from_files(files);

        let refusals = ["infra/index.md", "log.md", "AGENTS.md", "install/setup.md"]
            .map(|path| delete_concept(&workspace, path).unwrap_err());
        let missing = delete_concept(&workspace, "infra/none").unwrap_err();
        // Plan Decision 4: a machinery reference that names no file is
        // `NotFound`, since `check_path` runs only on a resolved path.
        let missing_machinery = delete_concept(&workspace, "install/none.md").unwrap_err();

        for error in &refusals {
            assert_eq!(error.kind(), "InvalidPath", "{error}");
        }
        assert!(
            refusals[3].to_string().contains("machinery"),
            "{}",
            refusals[3]
        );
        assert!(matches!(missing, OkfError::NotFound { .. }));
        assert!(matches!(missing_machinery, OkfError::NotFound { .. }));
    }

    #[test]
    fn each_link_on_a_line_is_its_own_entry() {
        let mut files = FILES.to_vec();
        files.push((
            "twice.md",
            "See [z](infra/z.md) and [z again](/infra/z.md#top).\n",
        ));
        let workspace = MemWorkspace::from_files(files);

        let error = delete_concept(&workspace, "infra/z.md").unwrap_err();

        assert!(
            matches!(
                error,
                OkfError::StillLinked { ref links, .. }
                    if links == &[link("twice.md", 1), link("twice.md", 1)]
            ),
            "{error:?}"
        );
    }
}
