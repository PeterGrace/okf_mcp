// Health (the landing page): five tiles from GET /catalogs/{c}/health, then
// one report table at a time, switched by a segmented control. A row opens
// its concept in Browse. Every number is computed by the server.
import { h, clear, pill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { ago, count } from '../core/format.js';
import { go, browseHash } from '../core/router.js';
import { listTable } from '../components/list-table.js';
import { segments } from '../components/segments.js';

// One row per link problem, broken links first.
function linkRows(report) {
    const issue = (kind, level) => item => ({ kind, level, path: item.file, line: item.line, detail: item.link });
    return [
        ...report.broken.map(issue('broken link', 'error')),
        ...report.broken_anchors.map(issue('broken anchor', 'error')),
        ...report.relative.map(issue('relative link', 'muted')),
        ...report.orphans.map(path => ({ kind: 'orphan', level: 'warn', path, line: null, detail: 'no concept links here' })),
        ...report.dead_ends.map(path => ({ kind: 'dead end', level: 'muted', path, line: null, detail: 'links to no other concept' })),
        ...report.index_drift.map(drift => ({ kind: 'index drift', level: 'warn', path: drift.file, line: null, detail: drift.problem })),
        ...report.unlinked_mentions.map(mention => ({
            kind: 'unlinked mention', level: 'info', path: mention.file, line: mention.hits[0]?.line ?? null,
            detail: `mentions ${mention.target} ${count(mention.hits.length, 'time')}`,
        })),
    ];
}

// Each report: its label, its rows from the API's JSON, the concept a row opens, its columns.
const REPORTS = {
    lint: {
        label: 'Lint',
        rows: report => [
            ...report.errors.map(finding => ({ ...finding, level: 'error' })),
            ...report.warnings.map(finding => ({ ...finding, level: 'warn' })),
        ],
        path: row => row.file,
        columns: [
            { key: 'level', label: 'Severity', render: row => pill(row.level === 'error' ? 'error' : 'warning', row.level), sort: row => row.level },
            { key: 'file', label: 'Path', class: 'mono', sort: row => row.file },
            { key: 'line', label: 'Line', sort: row => row.line },
            { key: 'rule', label: 'Rule', class: 'mono', sort: row => row.rule },
            { key: 'message', label: 'Message' },
        ],
    },
    sprawl: {
        label: 'Sprawl',
        rows: report => report.reports,
        path: row => row.path,
        columns: [
            { key: 'path', label: 'Path', class: 'mono', sort: row => row.path },
            { key: 'score', label: 'Score', sort: row => row.score },
            { key: 'lines', label: 'Lines', sort: row => row.lines },
            { key: 'sections', label: 'Sections', sort: row => row.sections },
            { key: 'volatile', label: 'Volatile', sort: row => row.volatile },
            { key: 'exempt', label: 'Exempt', render: row => row.exempt ?? '-' },
        ],
    },
    links: {
        label: 'Links',
        rows: linkRows,
        path: row => row.path,
        columns: [
            { key: 'kind', label: 'Problem', render: row => pill(row.kind, row.level), sort: row => row.kind },
            { key: 'path', label: 'Path', class: 'mono', sort: row => row.path },
            { key: 'line', label: 'Line', render: row => row.line ?? '-' },
            { key: 'detail', label: 'Detail' },
        ],
    },
    oldest: {
        label: 'Oldest',
        rows: report => report.rows,
        path: row => row.path,
        columns: [
            { key: 'path', label: 'Path', class: 'mono', sort: row => row.path },
            { key: 'title', label: 'Title', render: row => row.title ?? '-' },
            { key: 'last_touched', label: 'Last stamp', render: row => (row.last_touched ? `${ago(row.last_touched)} (${row.last_touched_by})` : 'never'), sort: row => row.last_touched ?? '' },
            { key: 'age_days', label: 'Age (days)', render: row => row.age_days ?? '-', sort: row => row.age_days ?? Infinity },
            { key: 'stale', label: 'Stale', render: row => (row.stale ? pill('stale', 'warn') : '-') },
        ],
    },
    questions: {
        label: 'Open questions',
        rows: report => report.concepts.flatMap(concept =>
            concept.questions.map(question => ({ path: concept.path, number: question.number, text: question.text }))),
        path: row => row.path,
        columns: [
            { key: 'path', label: 'Path', class: 'mono', sort: row => row.path },
            { key: 'number', label: '#' },
            { key: 'text', label: 'Question' },
        ],
    },
};

function tile(id, value, label, detail) {
    return h('div', { class: 'tile', id: `tile-${id}` },
        h('div', { class: 'n', id: `n-${id}` }, String(value)),
        h('div', { class: 'l' }, label),
        detail ? h('div', { class: 'l mono', id: `d-${id}` }, detail) : null);
}

async function loadTiles(catalog, tiles) {
    try {
        const counts = await api.get(`/catalogs/${enc(catalog)}/health`);
        const oldest = counts.oldest === null ? '-'
            : counts.oldest.age_days === null ? 'never'
                : `${Math.floor(counts.oldest.age_days)} d`;
        clear(tiles).append(
            tile('lint', counts.lint_errors, 'lint errors', count(counts.lint_warnings, 'warning')),
            tile('sprawl', counts.sprawl, 'sprawling concepts', null),
            tile('orphans', counts.orphans, 'orphans', count(counts.broken_links, 'broken link')),
            tile('oldest', oldest, 'oldest stamp', counts.oldest ? counts.oldest.path : 'no concepts'),
            tile('questions', counts.open_questions, 'open questions', null));
    } catch {
        clear(tiles).append(h('p', { class: 'muted' }, 'The counts could not be loaded.'));
    }
}

export async function render(main) {
    const catalog = store.catalog;
    main.append(h('header', { class: 'page-head' },
        h('div', {}, h('h1', {}, 'Health'), h('p', { class: 'muted' }, catalog ? `Catalog ${catalog}` : 'No catalog selected.')),
        catalog
            ? h('div', { class: 'actions' }, h('button', {
                type: 'button', id: 'health-refresh', class: 'secondary',
                onclick: () => { render(clear(main)); },
            }, 'Refresh'))
            : null));
    if (!catalog) return;

    const tiles = h('section', { class: 'tiles', id: 'health-tiles' }, h('p', { class: 'muted' }, 'Loading...'));
    const holder = h('section', { id: 'health-report' });
    const cache = new Map();   // report name -> the API's JSON, until Refresh
    let ticket = 0;            // a newer switch wins over a slower older fetch

    async function show(name) {
        const mine = ++ticket;
        const spec = REPORTS[name];
        clear(holder).append(h('p', { class: 'muted' }, 'Loading...'));
        try {
            if (!cache.has(name)) cache.set(name, await api.get(`/catalogs/${enc(catalog)}/reports/${name}`));
            if (mine !== ticket) return;
            const table = listTable(spec.columns, spec.rows(cache.get(name)), {
                onRow: row => go(browseHash(catalog, spec.path(row))),
                empty: 'Nothing to report.',
            });
            table.id = 'report-table';
            table.dataset.report = name;
            clear(holder).append(table);
        } catch {
            if (mine === ticket) clear(holder).append(h('p', { class: 'muted' }, 'This report could not be loaded.'));
        }
    }

    const switcher = segments(Object.entries(REPORTS).map(([value, spec]) => ({ value, label: spec.label })), 'lint', show);
    main.append(tiles, switcher, holder);
    await Promise.all([loadTiles(catalog, tiles), show('lint')]);
}
