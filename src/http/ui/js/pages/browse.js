// Browse: the catalog tree beside one document, read-only.
// Routes (each segment encoded; the router decodes them):
//   #/browse                                    the selected catalog's root index.md
//   #/browse/<catalog>[/<folder>...]            a folder's index.md
//   #/browse/<catalog>/<path>.md[/<slug>]       a concept or document, at a section
// A path ends at its first segment ending in '.md'; one more segment is a
// heading slug (the outline's, as src/render.rs writes links).
import { $, h, clear, pill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { go, browseHash } from '../core/router.js';
import { openDrawer } from '../core/drawer.js';
import { ago, when, count } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { segments } from '../components/segments.js';
import { tree } from '../components/tree.js';

// Read through /documents; everything else is a concept.
const DOCUMENTS = new Set(['index.md', 'log.md', 'AGENTS.md']);

// The shell keeps one #catalog-select for the page's lifetime (it only
// replaces its options), so one listener, added on the first render, serves
// every later render: a flag rather than a listener per navigation.
let switcherHooked = false;

function locate(rest) {
    const catalog = rest[0] ?? store.catalog;
    const parts = rest.slice(1);
    const file = parts.findIndex(part => part.endsWith('.md'));
    if (file === -1) {
        return { catalog, folder: parts.join('/'), path: [...parts, 'index.md'].join('/'), section: null };
    }
    return { catalog, folder: null, path: parts.slice(0, file + 1).join('/'), section: parts[file + 1] ?? null };
}

const encPath = path => path.split('/').map(enc).join('/');
const base = where => `/catalogs/${enc(where.catalog)}`;

export async function render(main, rest) {
    if (!switcherHooked) {
        // Switching catalogs while browsing opens the new catalog's root.
        $('#catalog-select').addEventListener('change', event => {
            if (location.hash.startsWith('#/browse')) go(browseHash(event.target.value));
        });
        switcherHooked = true;
    }
    const where = locate(rest);
    if (!where.catalog) {
        main.append(h('h1', {}, 'Browse'), h('p', { class: 'muted' }, 'This key reaches no catalog.'));
        return;
    }
    const side = h('aside', { class: 'browse-tree' }, h('p', { class: 'muted small' }, 'Loading...'));
    const view = h('section', { class: 'browse-view', id: 'browse-view' });
    main.append(h('div', { class: 'browse' }, side, view));
    let listing;
    try {
        listing = await api.get(`${base(where)}/tree`);
    } catch {
        clear(side);
        return;
    }
    clear(side).append(tree(listing.documents, { catalog: where.catalog, selected: `/${where.path}` }));
    const known = listing.documents.some(doc => doc.path === `/${where.path}`);
    if (where.folder !== null && !known) {
        showFolder(view, where, listing.documents);
        return;
    }
    if (DOCUMENTS.has(where.path.split('/').pop())) await showDocument(view, where);
    else await showConcept(view, where, false);
}

// A folder without index.md: its documents, as links.
function showFolder(view, where, documents) {
    const prefix = where.folder ? `/${where.folder}/` : '/';
    const inside = documents.filter(doc => doc.path.startsWith(prefix) && !doc.path.slice(prefix.length).includes('/'));
    clear(view).append(
        h('h1', { id: 'concept-title' }, prefix),
        h('p', { class: 'muted' }, 'This folder has no index.md.'),
        h('ul', {}, inside.map(doc => h('li', {}, h('a', { href: browseHash(where.catalog, doc.path) }, doc.title ?? doc.path)))));
}

// The page frame shared by concepts and documents: a header, segment tabs,
// and a pane each tab fills. A newer tab click wins over a slower older one.
function frame(view, { title, subtitle, extra, tabs }) {
    const pane = h('div', { id: 'concept-pane' });
    let ticket = 0;
    const show = async value => {
        const mine = ++ticket;
        const node = await tabs.find(tab => tab.value === value).build();
        if (mine === ticket) clear(pane).append(node);
    };
    const switcher = segments(tabs.map(({ value, label }) => ({ value, label })), tabs[0].value, show);
    switcher.id = 'concept-tabs';
    clear(view).append(
        h('header', { class: 'page-head' },
            h('div', {},
                h('h1', { id: 'concept-title' }, title),
                h('p', { class: 'mono small muted' }, subtitle),
                extra)),
        switcher,
        pane);
    show(tabs[0].value);
}

async function showConcept(view, where, full) {
    let concept;
    try {
        concept = await api.get(`${base(where)}/concepts/${encPath(where.path)}${full ? '?full=true' : ''}`);
    } catch {
        clear(view).append(h('p', { class: 'muted' }, 'This concept could not be loaded.'));
        return;
    }
    frame(view, {
        title: concept.title ?? concept.path,
        subtitle: `${concept.catalog} ${concept.path}`,
        extra: provenance(concept.frontmatter),
        tabs: [
            { value: 'content', label: 'Content', build: () => content(concept, where, () => showConcept(view, where, true)) },
            { value: 'links', label: 'Links', build: () => linksTab(where) },
            { value: 'history', label: 'History', build: () => historyTab(view, where) },
            { value: 'raw', label: 'Raw', build: () => rawTab(concept.raw, concept.note) },
        ],
    });
}

async function showDocument(view, where) {
    let documentRead;
    try {
        documentRead = await api.get(`${base(where)}/documents/${encPath(where.path)}`);
    } catch {
        clear(view).append(h('p', { class: 'muted' }, 'This document could not be loaded.'));
        return;
    }
    frame(view, {
        title: documentRead.path,
        subtitle: documentRead.catalog,
        extra: null,
        tabs: [
            { value: 'content', label: 'Content', build: () => content({ body_html: documentRead.body_html, outline: [] }, where, null) },
            { value: 'history', label: 'History', build: () => historyTab(view, where) },
            { value: 'raw', label: 'Raw', build: () => rawTab(documentRead.text, null) },
        ],
    });
}

// "Generated 3 d ago by x - Reviewed ... " plus source and question counts,
// from the frontmatter keys generated, reviewed, sources, open_questions.
function provenance(frontmatter) {
    const fm = frontmatter ?? {};
    const stamps = [];
    for (const [key, label] of [['generated', 'Generated'], ['reviewed', 'Reviewed']]) {
        const stamp = fm[key];
        if (stamp && typeof stamp === 'object' && stamp.at) {
            stamps.push(`${label} ${ago(String(stamp.at))}${stamp.by ? ` by ${stamp.by}` : ''}`);
        }
    }
    const sources = Array.isArray(fm.sources) ? fm.sources.length : 0;
    const questions = Array.isArray(fm.open_questions) ? fm.open_questions.length : 0;
    return h('p', { id: 'provenance', class: 'small muted' },
        stamps.join(' - ') || 'Never stamped', ' ',
        pill(count(sources, 'source'), sources ? 'info' : 'muted'), ' ',
        pill(count(questions, 'open question'), questions ? 'warn' : 'muted'));
}

function outlineNav(outline, where) {
    return h('div', { class: 'outline', id: 'outline', 'aria-label': 'Outline' },
        h('ul', {}, outline.map(entry => h('li', { class: `level-${entry.level}` },
            h('a', { href: browseHash(where.catalog, where.path, entry.slug) }, entry.heading)))));
}

function content(item, where, loadFull) {
    const wrap = h('div', {});
    const outline = item.outline ?? [];
    if (item.body_html == null) {
        wrap.append(
            h('p', { class: 'muted', id: 'concept-truncated' }, item.note ?? 'The body is over the server limit.'),
            outlineNav(outline, where),
            h('button', { type: 'button', id: 'concept-full', onclick: loadFull }, 'Load the whole body'));
        return wrap;
    }
    if (outline.length > 1) wrap.append(outlineNav(outline, where));
    const body = h('article', { class: 'markdown', id: 'markdown-body' });
    // The console's only innerHTML. body_html is produced by src/render.rs,
    // which turns all raw HTML in the source into escaped text, drops style
    // attributes, and keeps only http(s)/mailto links and console routes
    // (javascript:, data: and the like become plain text). Every other
    // value on every page goes through textContent.
    body.innerHTML = item.body_html;
    wrap.append(body);
    if (where.section) {
        // After the pane is attached, so scrolling has a layout to scroll.
        requestAnimationFrame(() => body.querySelector(`[data-slug="${CSS.escape(where.section)}"]`)?.scrollIntoView({ block: 'start' }));
    }
    return wrap;
}

async function linksTab(where) {
    let data;
    try {
        data = await api.get(`${base(where)}/links/${encPath(where.path)}`);
    } catch {
        return h('p', { class: 'muted' }, 'Links could not be loaded.');
    }
    const open = path => go(browseHash(where.catalog, path));
    const out = listTable([
        { key: 'target', label: 'Target', class: 'mono', render: row => row.fragment ? `${row.target}#${row.fragment}` : row.target, sort: row => row.target },
        { key: 'title', label: 'Title' },
        { key: 'line', label: 'Line', sort: row => row.line },
        { key: 'broken', label: 'State', render: row => row.broken ? pill('broken', 'error') : pill('ok', 'ok') },
    ], data.links, { onRow: row => { if (!row.broken) open(row.target); }, empty: 'No links out.' });
    out.id = 'links-out';
    const back = listTable([
        { key: 'path', label: 'From', class: 'mono', sort: row => row.path },
        { key: 'title', label: 'Title' },
        { key: 'line', label: 'Line', sort: row => row.line },
    ], data.backlinks, { onRow: row => open(row.path), empty: 'Nothing links here.' });
    back.id = 'backlinks';
    return h('div', {}, h('h2', {}, 'Links out'), out, h('h2', {}, 'Backlinks'), back);
}

async function historyTab(view, where) {
    let data;
    try {
        data = await api.get(`${base(where)}/history/${encPath(where.path)}`);
    } catch {
        return h('p', { class: 'muted' }, 'History could not be loaded.');
    }
    const table = listTable([
        { key: 'revision', label: 'Rev', sort: row => row.revision },
        { key: 'op', label: 'Op', render: row => pill(row.op, row.op === 'delete' ? 'error' : 'muted') },
        { key: 'actor', label: 'Actor', class: 'mono' },
        { key: 'message', label: 'Message' },
        { key: 'at', label: 'When', render: row => ago(row.at), sort: row => row.at },
    ], data.revisions, { onRow: row => openRevision(view, where, row), empty: 'No revisions.' });
    table.id = 'history';
    return table;
}

// One revision in the drawer: its diff from the revision before, and its text.
// The drawer is outside this page's container, so it opens only while the
// page is still shown (view is inside it, and detached with it).
async function openRevision(view, where, row) {
    let revision;
    try {
        revision = await api.get(`${base(where)}/revisions/${row.revision}/${encPath(where.path)}`);
    } catch {
        return;
    }
    if (!view.isConnected) return;
    const lines = revision.diff.split('\n').map(line => h('span', {
        class: line.startsWith('+') && !line.startsWith('+++') ? 'add'
            : line.startsWith('-') && !line.startsWith('---') ? 'del' : null,
    }, `${line}\n`));
    openDrawer(`${revision.path} @ ${revision.revision}`, `${revision.op} by ${row.actor}, ${when(row.at)}: ${row.message}`,
        h('h3', {}, 'Changes from the previous revision'),
        h('pre', { class: 'diff', id: 'revision-diff' }, lines),
        revision.text === null
            ? h('p', { class: 'muted' }, 'No text at this revision (deleted, or not text).')
            : h('details', {}, h('summary', {}, 'Text at this revision'), h('pre', { class: 'raw', id: 'revision-text' }, revision.text)));
}

function rawTab(raw, note) {
    if (raw == null) return h('p', { class: 'muted' }, note ?? 'The raw text is only returned with the whole body.');
    return h('pre', { class: 'raw', id: 'concept-raw' }, raw);
}
