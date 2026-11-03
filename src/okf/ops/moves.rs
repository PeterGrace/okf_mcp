//! `move_concept`: port of `okf.py`'s `cmd_mv` (`:1524`). A concept moves
//! to a new path: its folder listing goes with it, and every link into the
//! old path is rewritten ([`rewrite_links`]).
//!
//! What publish replays (plan Decision 1): the old folder's [`ReplayOp::Unlist`],
//! the new folder's `ensure_index` and [`ReplayOp::ListBullet`], and the
//! [`ReplayOp::Relink`] of every `index.md` and `log.md` whose links
//! changed. The moved concept and every other file whose links changed are
//! free edits. The old path is removed, which `write::apply` stores as a
//! deletion.
//!
//! Divergences from `okf.py` (each where `okf.py` crashes):
//! - an unlisted concept whose frontmatter does not parse is
//!   [`OkfError::FrontmatterParse`] when its new listing is built (`okf.py`
//!   raises `YamlError`);
//! - a move into the root of a catalog without a root `index.md` is
//!   [`OkfError::NotFound`] (`okf.py`: `FileNotFoundError`).
//!
//! `okf.py` runs `git mv` for a tracked file and `os.rename` otherwise;
//! [`MemWorkspace::rename`] keeps the bytes and execute bit, as both do.

use crate::error::OkfError;
use crate::okf::doc::Doc;
use crate::okf::markdown::{find_links, resolve_link};
use crate::okf::ops::log_index::collapse_whitespace;
use crate::okf::ops::relink::{rewrite_links, Moved, Redirect};
use crate::okf::ops::replay::{insert_index_bullet_text, unlist_text, ReplayOp};
use crate::okf::ops::{ensure_index, OpOutcome};
use crate::okf::paths::{basename, dirname, join, new_concept_path, resolve_concept};
use crate::okf::pystr::py_truthy_str;
use crate::okf::workspace::{MemWorkspace, Workspace};

/// Port of `cmd_mv`: moves a concept and rewrites every link to it.
///
/// # Arguments
/// * `workspace` - the catalog (the write path's scratch copy of an overlay).
/// * `old` - the concept, `/dir/x.md`, `dir/x` or `/dir/x`.
/// * `new` - its new path, with or without `.md` and a leading `/`.
///
/// # Returns
/// The outcome: touched paths in `okf.py`'s order (the old path, now
/// removed, first), the replay ops of the index and log edits, the message
/// `moved /<old> -> /<new>`, and the notes (a non-kebab-case name, folder
/// indexes created).
///
/// # Errors
/// - As [`resolve_concept`] for `old`.
/// - As [`new_concept_path`] for `new` (outside the catalog, reserved,
///   machinery, taken).
/// - [`OkfError::FrontmatterParse`] for an unlisted concept whose
///   frontmatter does not parse.
/// - The workspace's read or write error.
///
/// The path checks come before anything is written. Run the op on a
/// scratch copy, as the write path does, and drop the copy on error.
pub fn move_concept(workspace: &MemWorkspace, old: &str, new: &str) -> Result<OpOutcome, OkfError> {
    let old = resolve_concept(workspace, old)?;
    let (new, note) = new_concept_path(workspace, new)?;
    let mut outcome = OpOutcome::default();
    // `Option` is iterable (zero or one item), so `extend` adds the note if any.
    outcome
        .notes
        .extend(note.map(|note| format!("note: {note}")));
    let (old_dir, new_dir) = (dirname(&old), dirname(&new));
    outcome.touch(&old);
    outcome.touch(&new);
    ensure_index(workspace, new_dir, &mut outcome)?;

    // The old folder's listing travels with the concept, summary included.
    let old_index = join(old_dir, "index.md");
    let mut carried = None;
    if old_dir != new_dir && workspace.exists(&old_index) {
        let text = workspace.read_text(&old_index)?;
        if let Some((rest, bullet)) = unlist_text(workspace, &text, &old_index, &old) {
            workspace.write_atomic(&old_index, &rest)?;
            let op = ReplayOp::Unlist {
                target: old.clone(),
                bullet: Some(bullet.clone()),
            };
            outcome.record(&old_index, op);
            carried = Some(bullet);
        }
    }

    workspace.rename(&old, &new)?;

    if old_dir != new_dir {
        let name = basename(&new);
        let bullet = match carried {
            Some(bullet) => relinked_bullet(workspace, &bullet, &old_index, &old, name),
            None => frontmatter_bullet(workspace, &new)?,
        };
        let new_index = join(new_dir, "index.md");
        let text = workspace.read_text(&new_index)?;
        // `okf.py` inserts without checking that the index lists it already
        // (plan Decision 15); the replay op skips an index that does.
        let listed = insert_index_bullet_text(&text, &bullet, None, false)
            .expect("without a section a bullet always finds a place");
        workspace.write_atomic(&new_index, &listed)?;
        let op = ReplayOp::ListBullet {
            target: new.clone(),
            bullet,
            section: None,
            create_section: false,
        };
        outcome.record(&new_index, op);
    }

    let redirect = Redirect {
        from: old.clone(),
        to: new.clone(),
        anchors: None,
    };
    let moved = Moved {
        path: &new,
        written_at: &old,
    };
    let changed = rewrite_links(workspace, &redirect, Some(moved))?;
    outcome.record_rewrites(&changed, &redirect);
    outcome.message = format!("moved /{old} -> /{new}");
    Ok(outcome)
}

/// The old listing with its link into `old` pointing at `name` instead, the
/// fragment kept: `cmd_mv`'s `LINK_RE.sub` over the carried bullet.
///
/// As in `okf.py`, the first `(<target>` in the link's text is replaced, so
/// a link whose text itself holds `(<target>` changes there.
fn relinked_bullet(
    workspace: &dyn Workspace,
    bullet: &str,
    index: &str,
    old: &str,
    name: &str,
) -> String {
    let mut out = String::with_capacity(bullet.len());
    let mut copied = 0;
    for link in find_links(bullet) {
        // `is_none_or`: skip a link that does not resolve, or resolves elsewhere.
        if resolve_link(workspace, &link.target, index).is_none_or(|resolved| resolved.path != old)
        {
            continue;
        }
        let fragment = link
            .target
            .split_once('#')
            .map_or(String::new(), |(_, fragment)| format!("#{fragment}"));
        // `clone`: a `Range` is not `Copy`, and slicing would move it.
        let whole = &bullet[link.range.clone()];
        out.push_str(&bullet[copied..link.range.start]);
        out.push_str(&whole.replacen(
            &format!("({}", link.target),
            &format!("({name}{fragment}"),
            1,
        ));
        copied = link.range.end;
    }
    out.push_str(&bullet[copied..]);
    out
}

/// A listing built from the moved concept's frontmatter, for a concept its
/// old folder did not list: `* [<title, else the file stem>](<name>) -
/// <description, whitespace collapsed>`.
///
/// # Errors
/// [`OkfError::FrontmatterParse`] when the frontmatter does not parse; the
/// workspace's read error.
fn frontmatter_bullet(workspace: &dyn Workspace, new: &str) -> Result<String, OkfError> {
    let data = Doc::from_text(new, &workspace.read_to_string(new)?).parse_checked()?;
    let name = basename(new);
    // `data.get("title") or basename[:-3]`, formatted with `str()`.
    let title = py_truthy_str(data.get("title"))
        .unwrap_or_else(|| name.strip_suffix(".md").unwrap_or(name).to_string());
    let description = py_truthy_str(data.get("description")).unwrap_or_default();
    Ok(format!(
        "* [{title}]({name}) - {}",
        collapse_whitespace(&description)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::ops::replay::replay;
    use std::collections::BTreeMap;

    const INFRA_INDEX: &str =
        "# Infra\n\n# Concepts\n\n* [Time](time.md) - the clock\n* [NTP](ntp.md) - sync\n";
    const ROOT_INDEX: &str = "# Root\n\n# Sections\n\n* [Infra](infra/) - systems\n";

    const FILES: [(&str, &str); 7] = [
        ("index.md", ROOT_INDEX),
        ("infra/index.md", INFRA_INDEX),
        (
            "infra/time.md",
            "---\ntype: System\ntitle: Time\n---\nSee [NTP](ntp.md).\n",
        ),
        (
            "infra/ntp.md",
            "---\ntype: System\ntitle: NTP\ndescription: How   hosts sync.\n---\n\
             Uses [time](/infra/time.md#ports).\n",
        ),
        (
            "infra/unlisted.md",
            "---\ntype: Note\ntitle: Unlisted\ndescription: Not   listed.\n---\nAlone.\n",
        ),
        ("people/index.md", "# People\n"),
        ("log.md", "# Log\n"),
    ];

    fn text(workspace: &MemWorkspace, path: &str) -> String {
        workspace.read_to_string(path).unwrap()
    }

    #[test]
    fn a_move_within_a_folder_relinks_its_listing_and_every_link() {
        let workspace = MemWorkspace::from_files(FILES);

        let outcome = move_concept(&workspace, "infra/time.md", "infra/clock.md").unwrap();

        assert_eq!(outcome.message, "moved /infra/time.md -> /infra/clock.md");
        assert_eq!(
            outcome.touched,
            [
                "infra/time.md",
                "infra/clock.md",
                "infra/index.md",
                "infra/ntp.md"
            ]
        );
        assert!(outcome.notes.is_empty());
        assert!(!workspace.is_file("infra/time.md"));
        assert_eq!(
            text(&workspace, "infra/clock.md"),
            "---\ntype: System\ntitle: Time\n---\nSee [NTP](/infra/ntp.md).\n"
        );
        assert_eq!(
            text(&workspace, "infra/index.md"),
            "# Infra\n\n# Concepts\n\n* [Time](clock.md) - the clock\n* [NTP](ntp.md) - sync\n"
        );
        assert_eq!(
            text(&workspace, "infra/ntp.md"),
            "---\ntype: System\ntitle: NTP\ndescription: How   hosts sync.\n---\n\
             Uses [time](/infra/clock.md#ports).\n"
        );
        let relink = ReplayOp::Relink {
            redirect: Redirect {
                from: "infra/time.md".to_string(),
                to: "infra/clock.md".to_string(),
                anchors: None,
            },
        };
        assert_eq!(
            outcome.replay,
            BTreeMap::from([("infra/index.md".to_string(), vec![relink])])
        );
    }

    #[test]
    fn a_move_to_a_new_folder_carries_its_listing() {
        let workspace = MemWorkspace::from_files(FILES);

        let outcome = move_concept(&workspace, "/infra/ntp", "ops/net/ntp.md").unwrap();

        assert_eq!(outcome.message, "moved /infra/ntp.md -> /ops/net/ntp.md");
        assert_eq!(
            outcome.touched,
            [
                "infra/ntp.md",
                "ops/net/ntp.md",
                "ops/net/index.md",
                "ops/index.md",
                "index.md",
                "infra/index.md",
                "infra/time.md"
            ]
        );
        assert_eq!(
            outcome.notes,
            [
                "created /ops/index.md; replace its TODO lines (and the parent's listing)",
                "created /ops/net/index.md; replace its TODO lines (and the parent's listing)"
            ]
        );
        assert_eq!(
            text(&workspace, "infra/index.md"),
            "# Infra\n\n# Concepts\n\n* [Time](time.md) - the clock\n"
        );
        assert_eq!(
            text(&workspace, "ops/net/index.md"),
            "# Net\n\nTODO: say what this folder holds.\n\n# Concepts\n\n* [NTP](ntp.md) - sync\n"
        );
        // A relative link stays relative, through `relpath`.
        assert_eq!(
            text(&workspace, "infra/time.md"),
            "---\ntype: System\ntitle: Time\n---\nSee [NTP](../ops/net/ntp.md).\n"
        );
        assert_eq!(
            outcome.replay["infra/index.md"],
            [ReplayOp::Unlist {
                target: "infra/ntp.md".to_string(),
                bullet: Some("* [NTP](ntp.md) - sync".to_string()),
            }]
        );
        assert_eq!(
            outcome.replay["ops/net/index.md"],
            [
                ReplayOp::CreateIndex {
                    folder: "ops/net".to_string()
                },
                ReplayOp::ListBullet {
                    target: "ops/net/ntp.md".to_string(),
                    bullet: "* [NTP](ntp.md) - sync".to_string(),
                    section: None,
                    create_section: false,
                },
            ]
        );
        // The moved concept and the file whose link changed are free edits.
        assert!(!outcome.replay.contains_key("ops/net/ntp.md"));
        assert!(!outcome.replay.contains_key("infra/time.md"));
    }

    #[test]
    fn an_unlisted_concept_is_listed_from_its_frontmatter() {
        let workspace = MemWorkspace::from_files(FILES);

        let outcome = move_concept(&workspace, "infra/unlisted.md", "people/Unlisted.md").unwrap();

        assert_eq!(
            outcome.touched,
            ["infra/unlisted.md", "people/Unlisted.md", "people/index.md"]
        );
        assert_eq!(
            outcome.notes,
            ["note: Unlisted.md isn't short, lowercase and hyphenated"]
        );
        assert_eq!(
            text(&workspace, "people/index.md"),
            "# People\n\n# Concepts\n\n* [Unlisted](Unlisted.md) - Not listed.\n"
        );
        assert_eq!(text(&workspace, "infra/index.md"), INFRA_INDEX);
    }

    #[test]
    fn every_bookkeeping_edit_replays_onto_the_text_it_was_made_on() {
        let workspace = MemWorkspace::from_files(FILES);
        let original: BTreeMap<&str, &str> = FILES.into_iter().collect();

        let outcome = move_concept(&workspace, "infra/ntp.md", "ops/net/ntp.md").unwrap();

        assert_eq!(outcome.replay.len(), 4, "{:?}", outcome.replay.keys());
        for (path, ops) in &outcome.replay {
            // `copied`: the map holds `&str`s; `get` gives `Option<&&str>`.
            let before = original.get(path.as_str()).copied();
            let replayed = replay(path, before, ops).unwrap();
            assert_eq!(replayed, text(&workspace, path), "{path}");
        }
    }

    #[test]
    fn a_taken_reserved_or_missing_path_is_refused_before_anything_is_written() {
        let workspace = MemWorkspace::from_files(FILES);

        let taken = move_concept(&workspace, "infra/time.md", "infra/ntp.md").unwrap_err();
        let reserved = move_concept(&workspace, "infra/time.md", "infra/index.md").unwrap_err();
        let missing = move_concept(&workspace, "infra/none.md", "infra/x.md").unwrap_err();

        assert_eq!(taken.to_string(), "infra/ntp.md: already exists");
        assert_eq!(
            reserved.to_string(),
            "infra/index.md: index.md is a reserved name, not a concept"
        );
        assert!(matches!(missing, OkfError::NotFound { .. }));
        assert_eq!(text(&workspace, "infra/index.md"), INFRA_INDEX);
        assert!(workspace.is_file("infra/time.md"));
    }
}
