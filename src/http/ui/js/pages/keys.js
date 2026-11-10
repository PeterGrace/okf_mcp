// API keys (admin): the keys this admin manages, a form to create one (its
// secret shown once), and per-key grant, ungrant, reporting and revoke.
// The server enforces every rule; this page only hides what cannot work.
import { h, pill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../core/toast.js';
import { openDrawer, closeDrawer } from '../core/drawer.js';
import { when } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { confirmDialog } from '../components/confirm.js';

const LEVELS = ['read', 'write', 'admin'];
const ACCESS_KIND = { read: 'muted', write: 'info', admin: 'warn' };

function catalogsOf(key) {
    if (key.all_catalogs) return pill('all', 'info');
    return key.catalogs.length ? key.catalogs.join(', ') : '-';
}

function isMine(key) {
    return key.id === store.whoami?.key_id;
}

const COLUMNS = [
    { key: 'name', label: 'Name', sort: key => key.name,
      render: key => h('span', { class: 'key-name', 'data-test': `key-${key.name}` }, key.name, isMine(key) ? [' ', pill('you', 'info')] : null) },
    { key: 'actor', label: 'Actor', class: 'mono', sort: key => key.actor },
    { key: 'access', label: 'Access', sort: key => LEVELS.indexOf(key.access),
      render: key => pill(key.access, ACCESS_KIND[key.access]) },
    { key: 'catalogs', label: 'Catalogs', render: catalogsOf },
    { key: 'default_catalog', label: 'Default', class: 'mono' },
    { key: 'maintenance_report', label: 'Reporting',
      render: key => key.maintenance_report ? pill('reports', 'ok') : '-' },
    { key: 'created_at', label: 'Created', sort: key => key.created_at, render: key => when(key.created_at) },
    { key: 'revoked_at', label: 'State', sort: key => key.revoked_at ?? '',
      render: key => key.revoked_at
          ? [pill('revoked', 'error'), ' ', h('span', { class: 'muted small' }, when(key.revoked_at))]
          : pill('active', 'ok') },
];

export async function render(main) {
    const head = h('header', { class: 'page-head' },
        h('div', {},
            h('h1', {}, 'API keys'),
            h('p', { class: 'muted' }, 'The keys inside your reach. A secret is shown once, when its key is created.')));
    main.append(head);
    if (!store.can('admin')) {
        main.append(h('p', { class: 'muted', id: 'keys-denied' }, 'Managing keys needs an admin key.'));
        return;
    }
    const showRevoked = h('input', { type: 'checkbox', id: 'keys-show-revoked' });
    const table = listTable(COLUMNS, [], { onRow: key => showKey(key, load), empty: 'No keys.' });
    table.id = 'keys-table';
    const applyFilter = () => table.setFilter(showRevoked.checked ? null : key => !key.revoked_at);
    showRevoked.addEventListener('change', applyFilter);
    head.append(h('div', { class: 'actions' },
        h('button', { type: 'button', id: 'key-new', onclick: () => newKey(load) }, 'New key')));
    main.append(h('label', { class: 'small muted' }, showRevoked, ' Show revoked keys'), table);

    async function load() {
        try {
            table.redraw((await api.get('/keys')).keys);
        } catch { /* toasted; a 401 has already signed the browser out */ }
    }
    applyFilter();
    await load();
}

// The create form, in the drawer.
function newKey(reload) {
    const me = store.whoami;
    const form = h('form', { id: 'key-form' },
        h('label', {}, 'Name', h('input', { name: 'name', required: true, autocomplete: 'off', placeholder: 'letters, digits, . _ -' })),
        h('label', {}, 'Actor', h('input', { name: 'actor', autocomplete: 'off', placeholder: 'defaults to the name' })),
        h('label', {}, 'Access', h('select', { name: 'access' }, LEVELS.map(level => h('option', { value: level }, level)))),
        h('fieldset', {},
            h('legend', {}, 'Catalogs'),
            me.all_catalogs
                ? h('label', {}, h('input', { type: 'checkbox', name: 'all' }), ' Every catalog, present and future')
                : null,
            me.catalogs.map(name => h('label', {},
                h('input', { type: 'checkbox', name: 'catalog', value: name }), ' ', h('span', { class: 'mono' }, name)))),
        h('label', {}, 'Default catalog',
            h('select', { name: 'default_catalog' },
                h('option', { value: '' }, 'None'),
                me.catalogs.map(name => h('option', { value: name }, name)))),
        h('label', {},
            h('input', { type: 'checkbox', name: 'maintenance_report', role: 'switch' }), ' May submit maintenance reports'),
        h('button', { type: 'submit' }, 'Create key'));
    form.addEventListener('submit', async event => {
        event.preventDefault();
        const data = new FormData(form);
        const all = data.get('all') === 'on';
        const body = {
            name: String(data.get('name') ?? '').trim(),
            access: String(data.get('access') ?? 'read'),
            all,
            catalogs: all ? [] : data.getAll('catalog').map(String),
            default_catalog: String(data.get('default_catalog') ?? '') || null,
            actor: String(data.get('actor') ?? '').trim() || null,
            maintenance_report: data.get('maintenance_report') === 'on',
        };
        try {
            const created = await api.post('/keys', body);
            closeDrawer();
            showSecret(created);
            await reload();
        } catch { /* the server's reason is toasted; the form stays filled */ }
    });
    openDrawer('New key', 'The secret is shown once, right after creation.', form);
}

// The one time a secret is on screen: a modal that only the explicit
// button closes, after which the secret is removed from the page.
function showSecret(created) {
    const code = h('code', { id: 'key-secret' }, created.secret);
    const copy = h('button', { type: 'button', id: 'key-secret-copy', class: 'secondary' }, 'Copy');
    const done = h('button', { type: 'button', id: 'key-secret-done' }, 'I have stored it');
    const dialog = h('dialog', { id: 'key-secret-dialog', class: 'confirm' },
        h('article', {},
            h('h3', {}, `Key ${created.key.name} created`),
            h('p', { class: 'warn-text' }, 'Copy this key now. It is not stored anywhere and cannot be shown again.'),
            h('pre', { class: 'secret' }, code),
            h('footer', {}, copy, done)));
    copy.addEventListener('click', async () => {
        try {
            // navigator.clipboard exists only in secure contexts (https or localhost).
            await navigator.clipboard.writeText(created.secret);
            toast('Copied to the clipboard.', 'ok');
        } catch {
            toast('Copying failed: select the key and copy it by hand.', 'error');
        }
    });
    done.addEventListener('click', () => {
        code.textContent = '';
        dialog.close();
        dialog.remove();
    });
    dialog.addEventListener('cancel', event => event.preventDefault()); // Escape does not close it
    document.body.append(dialog);
    dialog.showModal();
}

function fact(label, value) {
    return [h('dt', {}, label), h('dd', {}, value)];
}

// One key's drawer: its facts, then grant, ungrant, reporting and revoke.
function showKey(key, reload) {
    const me = store.whoami;
    const mine = isMine(key);
    const facts = h('dl', { class: 'facts' },
        fact('Id', h('code', {}, key.id)),
        fact('Actor', h('code', {}, key.actor)),
        fact('Access', pill(key.access, ACCESS_KIND[key.access])),
        fact('Catalogs', catalogsOf(key)),
        fact('Default', key.default_catalog ?? '-'),
        fact('Reporting', key.maintenance_report ? 'may submit maintenance reports' : 'no'),
        fact('Created', when(key.created_at)),
        fact('Revoked', key.revoked_at ? when(key.revoked_at) : 'no'));
    const subtitle = mine ? 'The key this browser is signed in with' : `${key.access} key`;
    if (key.revoked_at) {
        openDrawer(key.name, subtitle, facts,
            h('p', { class: 'muted' }, 'This key is revoked: it authenticates nothing and cannot be changed.'));
        return;
    }
    const change = async (request, message) => {
        try {
            const updated = await request();
            toast(message, 'ok');
            await reload();
            showKey(updated, reload);
        } catch { /* toasted */ }
    };
    const base = `/keys/${enc(key.id)}`;
    let grants;
    if (key.all_catalogs) {
        grants = h('p', { class: 'muted small' }, 'Granted every catalog: per-catalog grants do not apply.');
    } else {
        const grantable = me.catalogs.filter(name => !key.catalogs.includes(name));
        const select = h('select', { id: 'key-grant-select', 'aria-label': 'Catalog to grant' },
            grantable.map(name => h('option', { value: name }, name)));
        grants = h('section', {},
            h('h3', {}, 'Catalogs'),
            h('ul', { class: 'grant-list' }, key.catalogs.length
                ? key.catalogs.map(name => h('li', {},
                    h('span', { class: 'mono' }, name), ' ',
                    h('button', {
                        type: 'button', class: 'text-btn', dataset: { ungrant: name },
                        onclick: () => change(() => api.del(`${base}/catalogs/${enc(name)}`), `Removed ${name} from ${key.name}.`),
                    }, 'Remove')))
                : h('li', { class: 'muted' }, 'No catalogs: this key reaches nothing.')),
            grantable.length
                ? h('div', { class: 'inline-form' }, select,
                    h('button', {
                        type: 'button', id: 'key-grant',
                        onclick: () => change(() => api.put(`${base}/catalogs/${enc(select.value)}`), `Granted ${select.value} to ${key.name}.`),
                    }, 'Grant'))
                : null);
    }
    const report = h('button', {
        type: 'button', id: 'key-report-toggle', class: 'secondary',
        onclick: () => change(() => api.patch(base, { maintenance_report: !key.maintenance_report }),
            key.maintenance_report ? `${key.name} may no longer report.` : `${key.name} may now report.`),
    }, key.maintenance_report ? 'Disable reporting' : 'Enable reporting');
    const revoke = h('button', {
        type: 'button', id: 'key-revoke', class: 'danger',
        onclick: async () => {
            const ok = await confirmDialog({
                title: `Revoke ${key.name}?`,
                message: mine
                    ? 'This is the key this browser is signed in with: revoking it signs you out at once. This cannot be undone.'
                    : 'Anything using this key is refused from its next request. This cannot be undone.',
                action: 'Revoke',
            });
            if (!ok) return;
            try {
                await api.post(`${base}/revoke`);
                closeDrawer();
                toast(`Revoked ${key.name}.`, 'ok');
                await reload(); // for the signed-in key this is a 401: back to the login card
            } catch { /* toasted */ }
        },
    }, 'Revoke');
    openDrawer(key.name, subtitle, facts, grants, h('div', { class: 'drawer-actions' }, report, revoke));
}
