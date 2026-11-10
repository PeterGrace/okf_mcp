// Maintenance findings: a table filtered by current disposition, a drawer
// with the claim, its evidence at the pinned revisions and the decision
// history, and for write keys the four disposition buttons. A decision is
// recorded only; it never edits the catalog. Also owns the nav badge.
import { h, pill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../core/toast.js';
import { openDrawer, closeDrawer } from '../core/drawer.js';
import { ago, when } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { segments } from '../components/segments.js';
import { browseHash } from '../core/router.js';

export const DISPOSITIONS = [
    { value: 'open', label: 'Open', kind: 'warn' },
    { value: 'needs_input', label: 'Needs input', kind: 'info' },
    { value: 'accepted_for_follow_up', label: 'Accepted', kind: 'ok' },
    { value: 'dismissed', label: 'Dismissed', kind: 'muted' },
];
const PAGE_SIZE = 200;

export function maintenancePath(catalog, rest) {
    return `/catalogs/${enc(catalog)}/maintenance${rest}`;
}

export function dispositionPill(value) {
    const known = DISPOSITIONS.find(d => d.value === value);
    return pill(known?.label ?? value, known?.kind ?? 'muted');
}

const words = text => String(text ?? '').replaceAll('_', ' ');
const conceptOf = finding => finding.input.evidence[0]?.pin.path ?? null;
const outdatedPill = finding => (finding.outdated ? [' ', pill('outdated', 'error')] : null);

// Sets #findings-badge to the selected catalog's open findings: hidden at 0,
// or when the count cannot be read. main.js calls it after sign-in and on a
// catalog switch; this page calls it on load and after every decision. Each
// call reads the counts afresh, so a late answer is still current unless the
// catalog changed meanwhile, which the check after the await covers.
export async function refreshFindingsBadge() {
    const badge = document.getElementById('findings-badge');
    const catalog = store.catalog;
    if (!badge) return;
    if (!catalog || !store.whoami) { badge.hidden = true; return; }
    try {
        const counts = await api.get(maintenancePath(catalog, '/counts'), { quiet: true });
        if (catalog !== store.catalog) return;   // switched meanwhile: the newer call wins
        badge.textContent = String(counts.open);
        badge.hidden = counts.open === 0;
    } catch {
        badge.hidden = true;
    }
}

// A retry key. randomUUID needs a secure context (https or localhost);
// getRandomValues works everywhere.
function newKey() {
    if (typeof crypto.randomUUID === 'function') return crypto.randomUUID();
    return Array.from(crypto.getRandomValues(new Uint8Array(16)), b => b.toString(16).padStart(2, '0')).join('');
}

function columns() {
    return [
        { key: 'disposition', label: 'State', sort: f => f.disposition,
            render: f => h('span', {}, dispositionPill(f.disposition), outdatedPill(f)) },
        { key: 'concept', label: 'Concept', class: 'mono', render: f => conceptOf(f) ?? '-', sort: f => conceptOf(f) ?? '' },
        { key: 'claim', label: 'Claim', render: f => f.input.assertion },
        { key: 'category', label: 'Category', render: f => words(f.input.category), sort: f => f.input.category },
        { key: 'run_id', label: 'Run', class: 'mono small' },
        { key: 'created_at', label: 'Created', render: f => ago(f.created_at), sort: f => f.created_at },
    ];
}

// Opens the finding's drawer. `main` is the page's view: a decision whose
// answer arrives after the user navigated away leaves the drawer alone.
function showFinding(main, catalog, finding, onChange) {
    const body = openDrawer('Finding', finding.id);
    // The decision in flight: retrying the same decision reuses its key, so a
    // lost response cannot append it twice.
    let attempt = null;

    async function decide(disposition, rationale, buttons) {
        if (!attempt || attempt.disposition !== disposition || attempt.rationale !== rationale) {
            attempt = { disposition, rationale, key: newKey() };
        }
        buttons.forEach(button => { button.disabled = true; });
        let updated;
        try {
            updated = await api.post(maintenancePath(catalog, `/findings/${enc(finding.id)}/disposition`),
                { disposition, rationale, idempotency_key: attempt.key });
        } catch {
            buttons.forEach(button => { button.disabled = false; });   // the API showed why
            return;
        }
        attempt = null;
        toast(`Recorded: ${DISPOSITIONS.find(d => d.value === disposition).label}`, 'ok');
        // The count changed whichever page is showing now: a fresh read.
        refreshFindingsBadge();
        if (!main.isConnected) return;
        draw(updated);
        onChange();
    }

    function actions() {
        const rationale = h('textarea', { id: 'dispose-rationale', rows: 3, placeholder: 'Why (recorded with your key\'s actor)' });
        const buttons = DISPOSITIONS.map(({ value, label }) => h('button', {
            type: 'button', id: `dispose-${value}`, class: 'secondary',
            onclick: () => decide(value, rationale.value.trim(), buttons),
        }, label));
        return h('div', { id: 'dispose-actions' },
            h('h3', {}, 'Decide'),
            h('label', {}, 'Rationale', rationale),
            h('div', { class: 'dispose-buttons' }, buttons));
    }

    function draw(current) {
        const input = current.input;
        body.replaceChildren(h('div', { class: 'finding' },
            h('p', { id: 'finding-state' }, dispositionPill(current.disposition), outdatedPill(current),
                ' ', h('span', { class: 'muted small' }, `${words(input.category)}, ${words(input.relationship)}`)),
            h('h3', {}, 'Claim'),
            h('p', { id: 'finding-claim' }, input.assertion),
            h('dl', { class: 'facts' },
                h('dt', {}, 'Follow-up'), h('dd', {}, input.follow_up),
                h('dt', {}, 'Uncertainty'), h('dd', {}, input.uncertainty),
                h('dt', {}, 'Reported by'), h('dd', {}, `${current.actor}, ${when(current.created_at)}`),
                h('dt', {}, 'Run'), h('dd', {}, h('a', { href: `#/runs/${enc(current.run_id)}`, class: 'mono', onclick: closeDrawer }, current.run_id))),
            h('h3', {}, 'Evidence'),
            h('ul', { id: 'finding-evidence' }, input.evidence.map(item => h('li', {},
                h('a', { href: browseHash(catalog, item.pin.path), class: 'mono', onclick: closeDrawer }, item.pin.path),
                h('span', { class: 'muted small' }, ` pinned at revision ${item.pin.revision}`),
                h('pre', { class: 'diff' }, item.excerpt)))),
            input.external.length
                ? [h('h3', {}, 'External observations'),
                    h('ul', {}, input.external.map(o => h('li', {},
                        h('span', { class: 'mono' }, o.resource), ` (${o.attribution})`,
                        h('pre', { class: 'diff' }, o.excerpt))))]
                : null,
            h('h3', {}, 'History'),
            current.dispositions.length
                ? h('ol', { id: 'finding-history' }, current.dispositions.map(d => h('li', {},
                    dispositionPill(d.disposition), ` ${d.actor}, ${when(d.at)}`,
                    d.rationale ? h('p', { class: 'small' }, d.rationale) : null)))
                : h('p', { id: 'finding-history', class: 'muted' }, 'No decisions yet.'),
            store.can('write') ? actions() : h('p', { class: 'muted small' }, 'A write key can record a decision.')));
    }

    draw(finding);
}

async function openFinding(main, catalog, id, onChange) {
    let finding;
    try {
        finding = await api.get(maintenancePath(catalog, `/findings/${enc(id)}`));
    } catch {
        return;   // the API showed a toast
    }
    if (main.isConnected) showFinding(main, catalog, finding, onChange);
}

export async function render(main, rest) {
    const catalog = store.catalog;
    refreshFindingsBadge();
    if (!catalog) {
        main.append(h('h1', {}, 'Findings'), h('p', { class: 'muted' }, 'No catalog selected.'));
        return;
    }
    let filter = 'open';
    let rows = [];
    let next = null;
    let ticket = 0;   // a newer load wins over a slower older one

    // A function declaration is hoisted, so it can be referenced above; it
    // first runs at the `await load(true)` below, after `table` and `more` exist.
    async function load(reset) {
        const mine = ++ticket;
        const query = new URLSearchParams({ limit: String(PAGE_SIZE) });
        if (filter !== 'all') query.set('disposition', filter);
        if (!reset && next) query.set('after', next);
        try {
            const page = await api.get(`${maintenancePath(catalog, '/findings')}?${query}`);
            if (mine !== ticket) return;
            rows = reset ? page.items : rows.concat(page.items);
            next = page.next_after ?? null;
            table.redraw(rows);
            more.hidden = !next;
        } catch { /* the API showed a toast */ }
    }

    const table = listTable(columns(), rows, {
        onRow: finding => showFinding(main, catalog, finding, () => load(true)),
        empty: 'No findings here.',
    });
    table.id = 'findings-table';
    const more = h('button', { type: 'button', id: 'findings-more', class: 'secondary', hidden: true, onclick: () => load(false) }, 'Load more');
    const filterBar = segments([...DISPOSITIONS.map(({ value, label }) => ({ value, label })), { value: 'all', label: 'All' }],
        filter, value => { filter = value; load(true); });
    filterBar.id = 'findings-filter';
    main.append(
        h('header', { class: 'page-head' },
            h('div', {}, h('h1', {}, 'Findings'),
                h('p', { class: 'muted' }, `Report-only maintenance findings in ${catalog}, oldest first. A decision never edits the catalog.`)),
            h('div', { class: 'actions' }, h('button', { type: 'button', id: 'findings-refresh', class: 'secondary', onclick: () => load(true) }, 'Refresh'))),
        filterBar, table, more);
    await load(true);
    if (rest[0]) await openFinding(main, catalog, rest[0], () => load(true));
}
