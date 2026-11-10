// Changesets: the open changesets of the selected catalog (the key's own;
// an admin sees every key's), with owner, idle state and file count. The
// drawer shows the pending diff, computed by the server, and discards.
import { h, pill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../core/toast.js';
import { openDrawer, closeDrawer } from '../core/drawer.js';
import { ago, when, count } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { confirmDialog } from '../components/confirm.js';

const COLUMNS = [
    { key: 'changeset', label: 'Handle', class: 'mono',
      render: row => h('span', { class: 'changeset-handle', 'data-test': 'changeset-handle' }, row.changeset) },
    { key: 'catalog', label: 'Catalog', class: 'mono', sort: row => row.catalog },
    { key: 'owner', label: 'Owner', sort: row => row.key_name ?? '',
      render: row => [row.key_name ?? '(deleted key)', ' ', h('span', { class: 'muted small mono' }, row.actor)] },
    { key: 'created', label: 'Opened', sort: row => row.created, render: row => when(row.created) },
    { key: 'last_change', label: 'Last change', sort: row => row.last_change, render: row => ago(row.last_change) },
    { key: 'files', label: 'Files', sort: row => row.paths.length, render: row => count(row.paths.length, 'file') },
    { key: 'idle', label: 'State', sort: row => (row.idle ? 1 : 0),
      render: row => (row.idle ? pill('idle', 'warn') : pill('active', 'ok')) },
];

export async function render(main) {
    const catalog = store.catalog;
    const scope = store.can('admin') ? "Every key's open changesets" : "Your key's open changesets";
    const table = listTable(COLUMNS, [], { onRow: row => show(main, row, load), empty: 'No open changesets.' });
    table.id = 'changesets-table';
    main.append(
        h('header', { class: 'page-head' },
            h('div', {},
                h('h1', {}, 'Changesets'),
                h('p', { class: 'muted' }, `${scope} in ${catalog ?? 'every catalog'}. Idle ones have had no change within the idle timeout.`)),
            h('div', { class: 'actions' },
                h('button', { type: 'button', id: 'changesets-refresh', class: 'secondary', onclick: () => load() }, 'Refresh'))),
        table);

    async function load() {
        try {
            const listed = await api.get(`/changesets?catalog=${enc(catalog ?? '')}`);
            table.redraw(listed.changesets);
        } catch { /* toasted */ }
    }
    await load();
}

// One changeset's drawer: its diff (loaded on open) and Discard. `main` is
// the page's view: once it is detached, the user has moved on and the
// drawer is no longer this page's to fill.
async function show(main, row, reload) {
    const diff = h('div', { id: 'changeset-diff' }, h('p', { class: 'muted' }, 'Loading the diff...'));
    const discardButton = store.can('write')
        ? h('button', { type: 'button', id: 'changeset-discard', class: 'danger', onclick: () => discardOne(main, row, reload) }, 'Discard')
        : null;
    openDrawer(row.changeset,
        `${row.catalog} - ${row.key_name ?? 'deleted key'} (${row.actor}) - last change ${ago(row.last_change)}`,
        row.idle ? h('p', {}, pill('idle', 'warn'), ' No change within the idle timeout.') : null,
        diff,
        h('div', { class: 'drawer-actions' }, discardButton));
    let content;
    try {
        const pending = await api.get(`/changesets/${enc(row.changeset)}/diff`);
        content = pending.files.length
            ? pending.files.map(file => h('section', { class: 'diff-file' },
                h('h3', { class: 'mono small' }, `${file.change} ${file.path}`, ' ', file.stale ? pill('stale', 'warn') : null),
                // The diff is a text child (a text node, like textContent), never markup.
                h('pre', { class: 'diff' }, file.diff)))
            : [h('p', { class: 'muted' }, 'No pending files: the changeset is empty.')];
    } catch (error) {
        content = [h('p', { class: 'error' }, `The diff could not be loaded: ${error.message}`)];
    }
    if (!main.isConnected) return;
    diff.replaceChildren(...content);
}

async function discardOne(main, row, reload) {
    const ok = await confirmDialog({
        title: 'Discard this changeset?',
        message: `${count(row.paths.length, 'pending file')} in ${row.catalog} will be dropped, and the handle ${row.changeset} closed for good. This cannot be undone.`,
        action: 'Discard',
    });
    if (!ok) return;
    try {
        const result = await api.del(`/changesets/${enc(row.changeset)}`);
        if (!main.isConnected) return;
        closeDrawer();
        toast(`Discarded ${count(result.removed, 'file')} from ${result.changeset}.`, 'ok');
        await reload();
    } catch { /* toasted */ }
}
