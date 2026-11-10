//! Markdown to safe HTML for the web console (web console design, "Markdown
//! rendering"). The console inserts this HTML as markup, so everything that
//! could run script is removed here, on the server:
//!
//! - raw HTML in the source (blocks and inline tags) is shown as escaped
//!   text, an HTML block as a code block;
//! - a link may point only at `http`, `https`, `mailto`, or a document of the
//!   same catalog, which becomes a console route (`#/browse/<catalog>/<path>`,
//!   plus `/<slug>` for a `#fragment`); any other link is plain text;
//! - an image becomes a link to its `http(s)` URL (the console's CSP loads no
//!   outside images, and it serves no catalog files), else its alt text;
//! - table alignment is dropped, because pulldown-cmark writes it as a
//!   `style` attribute, which the console's CSP refuses;
//! - headings get `id="section-<slug>"` and `data-slug="<slug>"`, the slug
//!   being the outline's ([`crate::okf::outline::Outline`]), so a section
//!   link finds its heading. The prefix keeps a heading named `Main` or
//!   `Drawer` from taking the id of a console element.
//!
//! Only the body is rendered: the frontmatter is split off exactly as the
//! outline splits it ([`body_split`]).

use std::collections::{HashMap, HashSet};

use pulldown_cmark::{html, Alignment, CodeBlockKind, CowStr, Event, HeadingLevel, LinkType};
use pulldown_cmark::{Options, Parser, Tag, TagEnd};

use crate::okf::markdown::{body_split, headings, resolve_link, slugify};
use crate::okf::paths::escapes_root;
use crate::okf::pystr::py_unquote;
use crate::okf::workspace::Workspace;

/// The prefix of every heading's `id`.
pub const HEADING_ID_PREFIX: &str = "section-";

/// The schemes an outside link may use (compared case-insensitively).
const ALLOWED_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

/// The schemes an image may use; it is shown as a link to its URL.
const IMAGE_SCHEMES: [&str; 2] = ["http", "https"];

/// The `title` of a link into the catalog whose target does not exist.
pub const BROKEN_LINK_TITLE: &str = "Not in this catalog";

/// The document being rendered: where its relative links resolve.
// `<'a>`: the struct borrows the names and the workspace for as long as the
// render runs, instead of copying them.
pub struct Page<'a> {
    /// The catalog's name, the first segment of every console route.
    catalog: &'a str,
    /// The document's catalog-relative path (no leading `/`).
    path: &'a str,
    /// The catalog's files, to tell a folder link from a file link and a
    /// broken link from a live one.
    workspace: &'a dyn Workspace,
}

impl<'a> Page<'a> {
    /// Describes the document being rendered.
    ///
    /// # Arguments
    /// * `catalog` - the catalog's name.
    /// * `path` - the document's catalog-relative path, without a leading `/`.
    /// * `workspace` - the catalog's files (a loaded catalog's
    ///   [`crate::catalog::Loaded::workspace`]).
    pub fn new(catalog: &'a str, path: &'a str, workspace: &'a dyn Workspace) -> Self {
        Self {
            catalog,
            path,
            workspace,
        }
    }
}

/// What becomes of one link.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Destination {
    /// An allowed outside URL: written as it is.
    Keep,
    /// A document of the catalog: this console route, and whether the
    /// document is missing.
    Route { href: String, broken: bool },
    /// Anything else: the link text stays, as plain text.
    Text,
}

/// Whether an open link or image was written as an `<a>` (so its end is
/// written too) or dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    Kept,
    Dropped,
}

/// The markdown extensions the console renders: GitHub tables,
/// strikethrough and task lists. Heading attributes (`{#id}`) stay off, so
/// a heading's text and slug are the outline's.
fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

/// Renders a document's body to HTML that is safe to insert as markup.
///
/// # Arguments
/// * `text` - the whole file with `\n` line endings (frontmatter included),
///   as the snapshot's `text()` or a raw file passed through
///   `universal_newlines`.
/// * `page` - where the document lives, for its links.
///
/// # Returns
/// The HTML of the body (the frontmatter is not rendered).
///
/// # Examples
/// ```
/// use okf_mcp::okf::workspace::MemWorkspace;
/// use okf_mcp::render::{to_html, Page};
/// let files = MemWorkspace::from_files([("a.md", "# A\n"), ("b.md", "# B\n")]);
/// let html = to_html("---\ntitle: A\n---\n# A\n\nSee [B](b.md).\n", &Page::new("main", "a.md", &files));
/// assert!(html.contains(r#"<h1 id="section-a" data-slug="a">A</h1>"#));
/// assert!(html.contains(r##"<a href="#/browse/main/b.md">B</a>"##));
/// assert!(!html.contains("title: A"));
/// ```
pub fn to_html(text: &str, page: &Page<'_>) -> String {
    let (_, lines) = body_split(text);
    // The outline's headings and slugs, found on the same body lines.
    let outlined = headings(&lines);
    let body = lines.join("\n");
    // Byte offset where each body line starts, to map a heading's offset to
    // its line (a binary search, so the whole pass stays O(n log n)).
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(body.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let slug_at_line: HashMap<usize, &str> = outlined
        .iter()
        .map(|heading| (heading.index, heading.slug.as_str()))
        .collect();
    let mut taken: HashSet<String> = outlined
        .iter()
        .map(|heading| heading.slug.clone())
        .collect();
    let mut events: Vec<Event<'_>> = Vec::new();
    // One entry per open link or image, so its end event knows its fate.
    let mut open: Vec<Fate> = Vec::new();
    // A heading the outline does not list (setext, in a quote): where its
    // start event is in `events`, and its text so far.
    let mut unlisted: Option<(usize, String)> = None;
    // `into_offset_iter` yields each event with its byte range in `body`.
    for (event, range) in Parser::new_ext(&body, options()).into_offset_iter() {
        match event {
            // Raw HTML becomes text, which push_html escapes.
            Event::Html(raw) | Event::InlineHtml(raw) => events.push(Event::Text(raw)),
            Event::Start(Tag::HtmlBlock) => {
                events.push(Event::Start(Tag::CodeBlock(CodeBlockKind::Indented)));
            }
            Event::End(TagEnd::HtmlBlock) => events.push(Event::End(TagEnd::CodeBlock)),
            Event::Start(Tag::Table(alignments)) => {
                let plain = vec![Alignment::None; alignments.len()];
                events.push(Event::Start(Tag::Table(plain)));
            }
            Event::Start(Tag::Heading { level, .. }) => {
                // Never 0: `line_starts[0]` is 0, which every offset passes.
                let line = line_starts.partition_point(|&start| start <= range.start) - 1;
                match slug_at_line.get(&line) {
                    Some(slug) => events.push(heading(level, slug)),
                    None => {
                        unlisted = Some((events.len(), String::new()));
                        events.push(heading(level, ""));
                    }
                }
            }
            Event::End(TagEnd::Heading(level)) => {
                // `take` moves the value out and leaves `None` behind.
                if let Some((at, text)) = unlisted.take() {
                    let slug = unique(slugify(&text), &mut taken);
                    events[at] = heading(level, &slug);
                }
                events.push(Event::End(TagEnd::Heading(level)));
            }
            Event::Text(text) => {
                if let Some((_, heading_text)) = unlisted.as_mut() {
                    heading_text.push_str(&text);
                }
                events.push(Event::Text(text));
            }
            Event::Code(code) => {
                if let Some((_, heading_text)) = unlisted.as_mut() {
                    heading_text.push_str(&code);
                }
                events.push(Event::Code(code));
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => match destination(link_type, &dest_url, page) {
                Destination::Keep => {
                    open.push(Fate::Kept);
                    events.push(Event::Start(Tag::Link {
                        link_type,
                        dest_url,
                        title,
                        id,
                    }));
                }
                Destination::Route { href, broken } => {
                    open.push(Fate::Kept);
                    let title = if broken {
                        CowStr::Borrowed(BROKEN_LINK_TITLE)
                    } else {
                        title
                    };
                    events.push(Event::Start(Tag::Link {
                        link_type: LinkType::Inline,
                        dest_url: CowStr::from(href),
                        title,
                        id,
                    }));
                }
                Destination::Text => open.push(Fate::Dropped),
            },
            Event::Start(Tag::Image {
                dest_url, title, ..
            }) => {
                // An image inside a link stays its alt text: no `<a>` in an `<a>`.
                let in_link = open.contains(&Fate::Kept);
                if !in_link && has_scheme(&dest_url, &IMAGE_SCHEMES) {
                    open.push(Fate::Kept);
                    events.push(Event::Start(Tag::Link {
                        link_type: LinkType::Inline,
                        dest_url,
                        title,
                        id: CowStr::Borrowed(""),
                    }));
                } else {
                    open.push(Fate::Dropped);
                }
            }
            Event::End(TagEnd::Link) | Event::End(TagEnd::Image) => {
                if open.pop() == Some(Fate::Kept) {
                    events.push(Event::End(TagEnd::Link));
                }
            }
            other => events.push(other),
        }
    }
    let mut out = String::with_capacity(body.len() + body.len() / 2);
    html::push_html(&mut out, events.into_iter());
    out
}

/// A heading's start event with the console's id and the slug.
///
/// # Arguments
/// * `level` - `h1` to `h6`.
/// * `slug` - the outline's slug (`""` for a placeholder, replaced at the
///   heading's end).
///
/// # Returns
/// An owned event (`'static`: it borrows nothing), which fits a list of
/// events borrowing the body.
fn heading(level: HeadingLevel, slug: &str) -> Event<'static> {
    Event::Start(Tag::Heading {
        level,
        id: Some(CowStr::from(format!("{HEADING_ID_PREFIX}{slug}"))),
        classes: Vec::new(),
        attrs: vec![(
            CowStr::Borrowed("data-slug"),
            Some(CowStr::from(slug.to_string())),
        )],
    })
}

/// `base`, or `base-1`, `base-2`, ... : the first not yet taken, which is
/// then taken (the outline numbers repeated headings the same way).
fn unique(base: String, taken: &mut HashSet<String>) -> String {
    let slug = if taken.contains(&base) {
        (1..)
            .map(|n| format!("{base}-{n}"))
            .find(|candidate| !taken.contains(candidate))
            .expect("an unbounded range always has an untaken number")
    } else {
        base
    };
    taken.insert(slug.clone());
    slug
}

/// The URL's scheme, when it starts with one (`scheme ":"`, RFC 3986).
///
/// # Returns
/// The scheme as written; `None` for a relative URL or a bare `#fragment`.
fn scheme(url: &str) -> Option<&str> {
    let end = url.find(':')?;
    let candidate = &url[..end];
    let mut chars = candidate.chars();
    let first = chars.next()?;
    let valid = first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then_some(candidate)
}

/// Whether `url` is free of control characters and uses one of `schemes`.
/// Browsers drop tabs and newlines inside a URL and leading spaces, so
/// `jav&#9;ascript:` would run; a URL with any control character is refused.
fn has_scheme(url: &str, schemes: &[&str]) -> bool {
    !url.chars().any(|c| c.is_ascii_control())
        && scheme(url.trim_start_matches(' ')).is_some_and(|found| {
            schemes
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(found))
        })
}

/// Decides what becomes of a link.
///
/// # Arguments
/// * `link_type` - how it was written (an email autolink gets `mailto:`
///   from the writer).
/// * `url` - its destination as parsed (entities decoded).
/// * `page` - the document holding it.
///
/// # Returns
/// [`Destination::Keep`] for an allowed scheme; [`Destination::Route`] for
/// a `#fragment` or a relative link to a `.md` file inside the catalog;
/// [`Destination::Text`] for anything else (another scheme, a control
/// character, a protocol-relative `//host` URL, a path leaving the catalog,
/// a link to a file that is not markdown).
fn destination(link_type: LinkType, url: &str, page: &Page<'_>) -> Destination {
    if link_type == LinkType::Email {
        return Destination::Keep;
    }
    if url.chars().any(|c| c.is_ascii_control()) || url.starts_with("//") {
        return Destination::Text;
    }
    if scheme(url.trim_start_matches(' ')).is_some() {
        return if has_scheme(url, &ALLOWED_SCHEMES) {
            Destination::Keep
        } else {
            Destination::Text
        };
    }
    if let Some(fragment) = url.strip_prefix('#') {
        return Destination::Route {
            href: browse_route(page.catalog, page.path, Some(&section_slug(fragment))),
            broken: false,
        };
    }
    // `let ... else`: bind the `Some` value or leave the function.
    let Some(resolved) = resolve_link(page.workspace, url, page.path) else {
        return Destination::Text;
    };
    if escapes_root(&resolved.path) || !resolved.path.ends_with(".md") {
        return Destination::Text;
    }
    // The path stays literal, as `resolve_link` and the catalog's link
    // checks read it; only the fragment is decoded (see `section_slug`).
    let fragment = url
        .split_once('#')
        .map(|(_, fragment)| section_slug(fragment));
    Destination::Route {
        href: browse_route(page.catalog, &resolved.path, fragment.as_deref()),
        broken: !page.workspace.is_file(&resolved.path),
    }
}

/// The heading slug a link's `#fragment` names, read as the catalog's link
/// check reads it (`src/okf/analysis/links.rs`): percent-decoded once, then
/// lowercased. Decoding here keeps [`browse_route`] from encoding an
/// already-encoded fragment a second time (`%C3%A9` would become `%25C3%25A9`).
///
/// # Arguments
/// * `fragment` - the text after `#`, as written in the link.
///
/// # Returns
/// The slug to put in the console route.
fn section_slug(fragment: &str) -> String {
    py_unquote(fragment).to_lowercase()
}

/// The console route of a document, and optionally one of its sections:
/// `#/browse/<catalog>/<path segments>[/<slug>]`, each segment
/// percent-encoded (the console's router decodes each one).
///
/// # Arguments
/// * `catalog` - the catalog.
/// * `path` - the document's catalog-relative path (a leading `/` is ignored).
/// * `section` - a heading slug; `None` or `""` for the top.
///
/// # Returns
/// The route, with its leading `#`.
///
/// # Examples
/// ```
/// use okf_mcp::render::browse_route;
/// assert_eq!(browse_route("main", "/a b/c.md", Some("notes")), "#/browse/main/a%20b/c.md/notes");
/// ```
pub fn browse_route(catalog: &str, path: &str, section: Option<&str>) -> String {
    let mut route = String::with_capacity(16 + catalog.len() + path.len());
    route.push_str("#/browse/");
    push_segment(&mut route, catalog);
    for segment in path.trim_start_matches('/').split('/') {
        route.push('/');
        push_segment(&mut route, segment);
    }
    if let Some(section) = section.filter(|section| !section.is_empty()) {
        route.push('/');
        push_segment(&mut route, section);
    }
    route
}

/// Appends `segment` percent-encoded: every byte but the RFC 3986
/// unreserved ones (`A-Z a-z 0-9 - . _ ~`) becomes `%XX`.
fn push_segment(out: &mut String, segment: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::outline::Outline;
    use crate::okf::workspace::MemWorkspace;

    /// A small catalog: `systems/alpha.md` links around it.
    fn catalog() -> MemWorkspace {
        MemWorkspace::from_files([
            ("index.md", "# Main\n"),
            ("systems/index.md", "# Systems\n"),
            ("systems/alpha.md", "# Alpha\n"),
            ("systems/beta.md", "# Beta\n\n## Config\n"),
            ("tools/okf.py", "print()\n"),
        ])
    }

    /// Renders `text` as `systems/alpha.md` of `main`.
    fn render(text: &str) -> String {
        let files = catalog();
        to_html(text, &Page::new("main", "systems/alpha.md", &files))
    }

    #[test]
    fn raw_html_is_escaped_never_passed_through() {
        let html =
            render("<script>alert(1)</script>\n\nText <b>bold</b> <img src=x onerror=alert(2)>\n");

        assert!(
            html.contains("<pre><code>&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{html}"
        );
        assert!(html.contains("&lt;b&gt;bold&lt;/b&gt;"), "{html}");
        assert!(
            html.contains("&lt;img src=x onerror=alert(2)&gt;"),
            "{html}"
        );
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("<b>"), "{html}");
        assert!(!html.contains("<img"), "{html}");
    }

    #[test]
    fn script_schemes_in_links_and_images_become_text() {
        let html = render(
            "[click](javascript:alert(1)) ![pic](javascript:alert(2)) <javascript:alert(3)> \
             [upper](JAVASCRIPT:alert(4)) [data](data:text/html,hi) \
             [tab](jav&#x09;ascript:alert(5)) ![svg](data:image/svg+xml,x) [proto](//example.com/x)\n",
        );

        let lowered = html.to_lowercase();
        assert!(!lowered.contains("href=\"javascript"), "{html}");
        assert!(!lowered.contains("href=\"jav"), "{html}");
        assert!(!lowered.contains("href=\"data"), "{html}");
        assert!(!lowered.contains("href=\"//"), "{html}");
        assert!(!lowered.contains("<img"), "{html}");
        assert!(!html.contains("<a"), "no link survives: {html}");
        for text in ["click", "pic", "upper", "data", "tab", "svg", "proto"] {
            assert!(html.contains(text), "{text} stays as text: {html}");
        }
    }

    #[test]
    fn outside_links_keep_allowed_schemes_and_images_become_links() {
        let html = render(
            "[site](https://example.com/x \"Site\") [plain](http://example.com) \
             [mail](mailto:ops@example.com) <ops@example.com> ![logo](https://example.com/l.png)\n",
        );

        assert!(
            html.contains(r#"<a href="https://example.com/x" title="Site">site</a>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<a href="http://example.com">plain</a>"#),
            "{html}"
        );
        assert_eq!(
            html.matches(r#"href="mailto:ops@example.com""#).count(),
            2,
            "{html}"
        );
        assert!(
            html.contains(r#"<a href="https://example.com/l.png">logo</a>"#),
            "{html}"
        );
    }

    #[test]
    fn relative_links_become_console_routes() {
        let html = render(
            "[Beta](beta.md) [Home](../index.md) [Cfg](beta.md#config) [Here](#notes) \
             [Folder](./) [Abs](/systems/beta.md)\n",
        );

        for href in [
            "#/browse/main/systems/beta.md",
            "#/browse/main/index.md",
            "#/browse/main/systems/beta.md/config",
            "#/browse/main/systems/alpha.md/notes",
            "#/browse/main/systems/index.md",
        ] {
            assert!(
                html.contains(&format!("href=\"{href}\"")),
                "{href} in {html}"
            );
        }
        assert_eq!(
            html.matches("href=\"#/browse/main/systems/beta.md\"")
                .count(),
            2,
            "{html}"
        );
        assert!(!html.contains(BROKEN_LINK_TITLE), "{html}");
    }

    #[test]
    fn links_leaving_the_catalog_or_to_other_files_are_text_and_missing_ones_are_marked() {
        let html = render("[Out](../../etc/passwd.md) [Tool](../tools/okf.py) [Gone](gone.md)\n");

        assert!(!html.contains("passwd"), "{html}");
        assert!(!html.contains("okf.py"), "{html}");
        assert!(html.contains("Out") && html.contains("Tool"), "{html}");
        assert!(
            html.contains(&format!(
                "<a href=\"#/browse/main/systems/gone.md\" title=\"{BROKEN_LINK_TITLE}\">Gone</a>"
            )),
            "{html}"
        );
    }

    #[test]
    fn heading_ids_are_the_outline_slugs() {
        let text =
            "---\ntitle: A\n---\n# Alpha\n\n## Notes\n\n## Notes\n\n```\n# not a heading\n```\n\
                    ## Use `okf.py` [here](beta.md)\n";
        let html = render(text);

        let slugs: Vec<String> = Outline::of(text)
            .entries()
            .into_iter()
            .map(|entry| entry.slug)
            .collect();
        assert_eq!(slugs, ["alpha", "notes", "notes-1", "use-okfpy-here"]);
        for slug in &slugs {
            assert!(
                html.contains(&format!(
                    "id=\"{HEADING_ID_PREFIX}{slug}\" data-slug=\"{slug}\""
                )),
                "{slug} in {html}"
            );
        }
        assert_eq!(html.matches("data-slug=").count(), slugs.len(), "{html}");
    }

    #[test]
    fn headings_the_outline_skips_get_unused_slugs() {
        let html = render("Title\n=====\n\n# Title\n\n> # Quoted\n");

        assert!(
            html.contains(r#"<h1 id="section-title" data-slug="title">Title</h1>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"data-slug="title-1""#),
            "the setext heading: {html}"
        );
        assert!(html.contains(r#"data-slug="quoted""#), "{html}");
    }

    #[test]
    fn frontmatter_is_not_rendered() {
        let html = render("---\ntype: system\ntitle: Alpha\n---\n# Alpha\n\nBody.\n");

        assert!(!html.contains("type: system"), "{html}");
        assert!(!html.contains("<hr"), "{html}");
        assert!(html.contains("<p>Body.</p>"), "{html}");
    }

    #[test]
    fn tables_strikethrough_and_task_lists_render_without_inline_styles() {
        let html = render("| a | b |\n|:--|--:|\n| 1 | 2 |\n\n~~old~~\n\n- [x] done\n- [ ] open\n");

        assert!(html.contains("<table>"), "{html}");
        assert!(
            html.contains("<th>a</th>") && html.contains("<td>2</td>"),
            "{html}"
        );
        assert!(
            !html.contains("style="),
            "the CSP refuses style attributes: {html}"
        );
        assert!(html.contains("<del>old</del>"), "{html}");
        assert!(html.contains(r#"type="checkbox""#), "{html}");
    }

    #[test]
    fn encoded_fragments_are_decoded_once_and_paths_stay_literal() {
        let files = MemWorkspace::from_files([
            ("systems/alpha.md", "# Alpha\n"),
            ("systems/beta.md", "# Beta\n\n## Caf\u{e9}\n"),
            ("systems/a b.md", "# A b\n"),
            ("systems/a%20b.md", "# Literal\n"),
        ]);
        let html = to_html(
            "[Cafe](beta.md#caf%C3%A9) [Here](#Caf%C3%A9) [Upper](beta.md#CONFIG) \
             [Encoded](a%20b.md)\n",
            &Page::new("main", "systems/alpha.md", &files),
        );

        for href in [
            "#/browse/main/systems/beta.md/caf%C3%A9",
            "#/browse/main/systems/alpha.md/caf%C3%A9",
            "#/browse/main/systems/beta.md/config",
            // The file named `a%20b.md`, as the catalog's link check reads
            // the link: `%` is encoded once, so the router decodes it back.
            "#/browse/main/systems/a%2520b.md",
        ] {
            assert!(
                html.contains(&format!("href=\"{href}\"")),
                "{href} in {html}"
            );
        }
        assert!(!html.contains("%25C3"), "no double encoding: {html}");
        assert!(!html.contains(BROKEN_LINK_TITLE), "{html}");
    }

    #[test]
    fn routes_percent_encode_each_segment() {
        assert_eq!(
            browse_route("main", "a b/c#d.md", Some("x y")),
            "#/browse/main/a%20b/c%23d.md/x%20y"
        );
        assert_eq!(
            browse_route("main", "/index.md", Some("")),
            "#/browse/main/index.md"
        );
        assert_eq!(
            browse_route("main", "n\u{e9}.md", None),
            "#/browse/main/n%C3%A9.md"
        );
    }

    #[test]
    fn schemes_are_recognized_only_at_the_start() {
        assert_eq!(scheme("https://example.com"), Some("https"));
        assert_eq!(scheme("dir/a:b.md"), None);
        assert_eq!(scheme("#x:y"), None);
        assert!(has_scheme(" HTTPS://example.com", &ALLOWED_SCHEMES));
        assert!(!has_scheme("ht\ttps://example.com", &ALLOWED_SCHEMES));
    }
}
