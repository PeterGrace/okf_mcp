// Catalogs: the catalogs this key reaches. Admins edit descriptions and
// delete; an admin key that reaches every catalog also creates them.
import { $, h } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../core/toast.js';
import { openDrawer, closeDrawer } from '../core/drawer.js';
import { ago, count } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { confirmDialog } from '../components/confirm.js';
import { refreshIdentity } from '../core/shell.js';

export async function render(main) {
    const admin = store.can('admin');
    const creator = admin && store.whoami.all_catalogs;
    const columns = [
        { key: 'name', label: 'Name', sort: row => row.name,
          render: row => h('code', { dataset: { catalog: row.name } }, row.name) },
        { key: 'description', label: 'Description',
          render: row => row.description || h('span', { class: 'muted' }, '-') },
        { key: 'concepts', label: 'Concepts', sort: row => row.concepts, class: 'num' },
        { key: 'files', label: 'Files', sort: row => row.files, class: 'num' },
        { key: 'generation', label: 'Generation', sort: row => row.generation, class: 'num' },
        { key: 'last_change', label: 'Last change', sort: row => row.last_change ?? '',
          render: row => ago(row.last_change) },
    ];
    const table = listTable(columns, [], { onRow: admin ? openEdit : null, empty: 'This key reaches no catalog.' });

    async function load() {
        try { table.redraw((await api.get('/catalogs')).catalogs); } catch { /* toasted */ }
    }

    // After a create or delete: the switcher is shell state and must follow
    // the change even if the user has navigated away meanwhile, so it is
    // rebuilt first, before the table. The drawer belongs to whichever page
    // is shown now, so it is closed only while this page still is.
    async function settled() {
        await refreshIdentity().catch(() => {});
        if (!main.isConnected) return;
        closeDrawer();
        await load();
    }

    function openCreate() {
        const form = h('form', { id: 'catalog-create-form' },
            h('label', {}, 'Name',
                h('input', { name: 'name', required: true, pattern: '[a-z0-9-]+', autocomplete: 'off',
                             placeholder: 'lowercase letters, digits and -' })),
            h('label', {}, 'Description', h('textarea', { name: 'description', rows: '3' })),
            h('button', { type: 'submit' }, 'Create catalog'));
        form.addEventListener('submit', async event => {
            event.preventDefault();
            const data = new FormData(form);
            const name = String(data.get('name') ?? '').trim();
            try {
                await api.post('/catalogs', { name, description: String(data.get('description') ?? '') });
            } catch { return; /* toasted: invalid or taken name */ }
            toast(`Created catalog ${name}.`, 'ok');
            await settled();
        });
        openDrawer('New catalog', 'An empty catalog; import a .tar.gz into it next.', form);
    }

    function openEdit(row) {
        const description = h('textarea', { id: 'catalog-description', name: 'description', rows: '3' }, row.description);
        const edit = h('form', { id: 'catalog-edit-form' },
            h('label', {}, 'Description', description),
            h('button', { type: 'submit', id: 'catalog-save' }, 'Save description'));
        edit.addEventListener('submit', async event => {
            event.preventDefault();
            try {
                await api.patch(`/catalogs/${enc(row.name)}`, { description: description.value });
            } catch { return; }
            toast(`Saved ${row.name}'s description.`, 'ok');
            if (!main.isConnected) return;   // navigated away: the drawer is another page's now
            closeDrawer();
            await load();
        });
        const force = h('input', { type: 'checkbox', id: 'catalog-force' });
        const remove = h('button', { type: 'button', id: 'catalog-delete', class: 'danger' }, 'Delete catalog');
        remove.addEventListener('click', async () => {
            const ok = await confirmDialog({
                title: `Delete ${row.name}?`,
                message: 'Its documents, history, changesets and key grants are deleted. This cannot be undone.',
                action: 'Delete',
                typed: row.name,
            });
            if (!ok) return;
            try {
                await api.del(`/catalogs/${enc(row.name)}${force.checked ? '?force=true' : ''}`);
            } catch { return; /* toasted: 409 names ?force=true */ }
            toast(`Deleted catalog ${row.name}.`, 'ok');
            await settled();
        });
        openDrawer(row.name, `generation ${row.generation} - ${count(row.concepts, 'concept')}, ${count(row.files, 'file')}`,
            edit,
            h('section', { class: 'danger-zone' },
                h('h3', {}, 'Delete'),
                h('label', {}, force, ' Also discard unpublished changes (force)'),
                remove));
    }

    main.append(
        h('div', { class: 'page-head' },
            h('div', {}, h('h1', {}, 'Catalogs'), h('p', { class: 'muted' }, 'The catalogs this key reaches.')),
            h('div', { class: 'actions' },
                creator ? h('button', { type: 'button', id: 'catalog-create', onclick: openCreate }, 'New catalog') : null)),
        table,
        admin && !creator
            ? h('p', { class: 'muted small' }, 'Creating a catalog needs an admin key that reaches every catalog.')
            : null);
    await load();
}
