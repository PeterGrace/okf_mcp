// API keys (admin): the keys this admin manages, a form to create one (its
// secret shown once), and per-key grant, ungrant, reporting and revoke.
// Below them, OAuth sign-in codes: mint one (shown once), list, revoke.
// OAuth grants (the keys codes become) are marked in the key list.
// The server enforces every rule; this page only hides what cannot work.
// Each helper takes the page's `main` and checks `main.isConnected` after an
// await: once the user has navigated away, the drawer is another page's.
import { h, pill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../core/toast.js';
import { openDrawer, closeDrawer } from '../core/drawer.js';
import { when } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { confirmDialog } from '../components/confirm.js';

const LEVELS = ['read', 'write', 'admin'];
const CODE_LEVELS = ['read', 'write'];
const ACCESS_KIND = { read: 'muted', write: 'info', admin: 'warn' };
const CODE_STATE_KIND = { live: 'ok', expired: 'muted', redeemed: 'info', revoked: 'error' };
// humantime spellings the server parses; the default is a day.
const EXPIRIES = [['1h', '1 hour'], ['24h', '24 hours'], ['3d', '3 days'], ['7d', '7 days']];

// Keys and codes both carry all_catalogs and catalogs.
function catalogsOf(grantee) {
    if (grantee.all_catalogs) return pill('all', 'info');
    return grantee.catalogs.length ? grantee.catalogs.join(', ') : '-';
}

function isMine(key) {
    return key.id === store.whoami?.key_id;
}

function isGrant(key) {
    return key.kind === 'oauth';
}

const COLUMNS = [
    { key: 'name', label: 'Name', sort: key => key.name,
      render: key => h('span', { class: 'key-name', 'data-test': `key-${key.name}` }, key.name,
          isMine(key) ? [' ', pill('you', 'info')] : null,
          isGrant(key)
              ? [' ', pill('OAuth', 'info'), ' ', h('span', { class: 'muted small client-name' }, key.client_name ?? 'unknown client')]
              : null) },
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

const CODE_COLUMNS = [
    { key: 'name', label: 'Name', sort: code => code.name,
      render: code => h('span', { class: 'key-name', 'data-test': `code-${code.name}` }, code.name) },
    { key: 'actor', label: 'Actor', class: 'mono', sort: code => code.actor },
    { key: 'access', label: 'Access', sort: code => LEVELS.indexOf(code.access),
      render: code => pill(code.access, ACCESS_KIND[code.access]) },
    { key: 'catalogs', label: 'Catalogs', render: catalogsOf },
    { key: 'default_catalog', label: 'Default', class: 'mono' },
    { key: 'expires_at', label: 'Expires', sort: code => code.expires_at, render: code => when(code.expires_at) },
    { key: 'state', label: 'State', sort: code => code.state,
      render: code => h('span', { 'data-test': `code-state-${code.name}` }, pill(code.state, CODE_STATE_KIND[code.state])) },
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
    const table = listTable(COLUMNS, [], { onRow: key => showKey(main, key, load), empty: 'No keys.' });
    table.id = 'keys-table';
    const applyFilter = () => table.setFilter(showRevoked.checked ? null : key => !key.revoked_at);
    showRevoked.addEventListener('change', applyFilter);
    head.append(h('div', { class: 'actions' },
        h('button', { type: 'button', id: 'key-new', onclick: () => newKey(main, load) }, 'New key')));
    const codes = codesPanel(main);
    main.append(h('label', { class: 'small muted' }, showRevoked, ' Show revoked keys'), table, codes.section);

    async function load() {
        try {
            const { keys } = await api.get('/keys');
            if (main.isConnected) table.redraw(keys);
        } catch { /* toasted; a 401 has already signed the browser out */ }
    }
    applyFilter();
    await Promise.all([load(), codes.load()]);
}

// The catalog checkboxes and default select both forms share.
function catalogFields() {
    const me = store.whoami;
    return [
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
    ];
}

// The grant fields of a submitted form, as both create bodies spell them.
function readGrants(data) {
    const all = data.get('all') === 'on';
    return {
        all,
        catalogs: all ? [] : data.getAll('catalog').map(String),
        default_catalog: String(data.get('default_catalog') ?? '') || null,
    };
}

// The create form, in the drawer.
function newKey(main, reload) {
    const form = h('form', { id: 'key-form' },
        h('label', {}, 'Name', h('input', { name: 'name', required: true, autocomplete: 'off', placeholder: 'letters, digits, . _ -' })),
        h('label', {}, 'Actor', h('input', { name: 'actor', autocomplete: 'off', placeholder: 'defaults to the name' })),
        h('label', {}, 'Access', h('select', { name: 'access' }, LEVELS.map(level => h('option', { value: level }, level)))),
        catalogFields(),
        h('label', {},
            h('input', { type: 'checkbox', name: 'maintenance_report', role: 'switch' }), ' May submit maintenance reports'),
        h('button', { type: 'submit' }, 'Create key'));
    form.addEventListener('submit', async event => {
        event.preventDefault();
        const data = new FormData(form);
        const body = {
            name: String(data.get('name') ?? '').trim(),
            access: String(data.get('access') ?? 'read'),
            ...readGrants(data),
            actor: String(data.get('actor') ?? '').trim() || null,
            maintenance_report: data.get('maintenance_report') === 'on',
        };
        try {
            const created = await api.post('/keys', body);
            // The secret is shown even after navigating away: it exists only now.
            if (main.isConnected) closeDrawer();
            showOnce({
                prefix: 'key',
                title: `Key ${created.key.name} created`,
                warning: 'Copy this key now. It is not stored anywhere and cannot be shown again.',
                secret: created.secret,
            });
            if (main.isConnected) await reload();
        } catch { /* the server's reason is toasted; the form stays filled */ }
    });
    openDrawer('New key', 'The secret is shown once, right after creation.', form);
}

// The one time a secret is on screen: a modal that only the explicit
// button closes, after which the secret is removed from the page. Element
// ids start with `prefix` (`key-secret`, `code-secret-done`, ...). The
// secret lives only in this dialog: never logged, never stored.
function showOnce({ prefix, title, warning, secret }) {
    let shown = secret; // dropped on close, so not even the copy handler keeps it
    const code = h('code', { id: `${prefix}-secret` }, shown);
    const copy = h('button', { type: 'button', id: `${prefix}-secret-copy`, class: 'secondary' }, 'Copy');
    const done = h('button', { type: 'button', id: `${prefix}-secret-done` }, 'I have stored it');
    const dialog = h('dialog', { id: `${prefix}-secret-dialog`, class: 'confirm' },
        h('article', {},
            h('h3', {}, title),
            h('p', { class: 'warn-text' }, warning),
            h('pre', { class: 'secret' }, code),
            h('footer', {}, copy, done)));
    copy.addEventListener('click', async () => {
        try {
            // navigator.clipboard exists only in secure contexts (https or localhost).
            await navigator.clipboard.writeText(shown);
            toast('Copied to the clipboard.', 'ok');
        } catch {
            toast('Copying failed: select the text and copy it by hand.', 'error');
        }
    });
    done.addEventListener('click', () => {
        shown = null;
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
function showKey(main, key, reload) {
    const me = store.whoami;
    const mine = isMine(key);
    const grant = isGrant(key);
    const facts = h('dl', { class: 'facts' },
        fact('Id', h('code', {}, key.id)),
        fact('Kind', grant ? 'OAuth grant' : 'API key'),
        grant ? fact('Client', key.client_name ?? 'unknown client') : null,
        fact('Actor', h('code', {}, key.actor)),
        fact('Access', pill(key.access, ACCESS_KIND[key.access])),
        fact('Catalogs', catalogsOf(key)),
        fact('Default', key.default_catalog ?? '-'),
        fact('Reporting', key.maintenance_report ? 'may submit maintenance reports' : 'no'),
        fact('Created', when(key.created_at)),
        fact('Revoked', key.revoked_at ? when(key.revoked_at) : 'no'));
    const subtitle = mine
        ? 'The key this browser is signed in with'
        : grant ? `OAuth grant for ${key.client_name ?? 'an unknown client'}` : `${key.access} key`;
    if (key.revoked_at) {
        openDrawer(key.name, subtitle, facts,
            h('p', { class: 'muted' }, 'This key is revoked: it authenticates nothing and cannot be changed.'));
        return;
    }
    const change = async (request, message) => {
        try {
            const updated = await request();
            toast(message, 'ok');
            if (!main.isConnected) return;
            await reload();
            if (main.isConnected) showKey(main, updated, reload);
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
    // An OAuth grant never reports (the server refuses it), so no toggle.
    const report = grant ? null : h('button', {
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
                    : grant
                        ? 'The client is refused from its next request, and its tokens stop working. This cannot be undone.'
                        : 'Anything using this key is refused from its next request. This cannot be undone.',
                action: 'Revoke',
            });
            if (!ok) return;
            try {
                await api.post(`${base}/revoke`);
                toast(`Revoked ${key.name}.`, 'ok');
                if (!main.isConnected) return;
                closeDrawer();
                await reload(); // for the signed-in key this is a 401: back to the login card
            } catch { /* toasted */ }
        },
    }, 'Revoke');
    openDrawer(key.name, subtitle, facts, grants, h('div', { class: 'drawer-actions' }, report, revoke));
}

// The sign-in codes section: its own head, filter and table. Returns the
// section and its loader, which `render` awaits beside the keys'.
function codesPanel(main) {
    const showAll = h('input', { type: 'checkbox', id: 'codes-show-all' });
    const table = listTable(CODE_COLUMNS, [], { onRow: code => showCode(main, code, load), empty: 'No sign-in codes to show.' });
    table.id = 'codes-table';
    const applyFilter = () => table.setFilter(showAll.checked ? null : code => code.state === 'live');
    showAll.addEventListener('change', applyFilter);
    applyFilter();
    async function load() {
        try {
            const { codes } = await api.get('/oauth/codes');
            if (main.isConnected) table.redraw(codes);
        } catch { /* toasted */ }
    }
    const section = h('section', { id: 'codes-panel', class: 'codes-panel' },
        h('header', { class: 'page-head' },
            h('div', {},
                h('h2', {}, 'Sign-in codes'),
                h('p', { class: 'muted' },
                    'One-time codes a person enters when connecting an OAuth client such as ChatGPT. '
                    + 'Each becomes an OAuth grant with the code\'s catalogs, access and actor.')),
            h('div', { class: 'actions' },
                h('button', { type: 'button', id: 'code-new', onclick: () => newCode(main, load) }, 'New sign-in code'))),
        h('label', { class: 'small muted' }, showAll, ' Show redeemed, expired and revoked codes'),
        table);
    return { section, load };
}

// The mint form, in the drawer.
function newCode(main, reload) {
    const form = h('form', { id: 'code-form' },
        h('label', {}, 'Name', h('input', { name: 'name', required: true, autocomplete: 'off', placeholder: 'letters, digits, . _ -' })),
        h('label', {}, 'Actor', h('input', { name: 'actor', required: true, autocomplete: 'off', placeholder: 'who the grant writes as' })),
        h('label', {}, 'Access', h('select', { name: 'access' }, CODE_LEVELS.map(level => h('option', { value: level }, level)))),
        catalogFields(),
        h('label', {}, 'Expires unused after',
            h('select', { name: 'expires_in' },
                EXPIRIES.map(([value, label]) => h('option', { value, selected: value === '24h' }, label)))),
        h('button', { type: 'submit' }, 'Create sign-in code'));
    form.addEventListener('submit', async event => {
        event.preventDefault();
        const data = new FormData(form);
        const body = {
            name: String(data.get('name') ?? '').trim(),
            actor: String(data.get('actor') ?? '').trim(),
            access: String(data.get('access') ?? 'read'),
            ...readGrants(data),
            expires_in: String(data.get('expires_in') ?? '24h'),
        };
        try {
            const created = await api.post('/oauth/codes', body);
            if (main.isConnected) closeDrawer();
            showOnce({
                prefix: 'code',
                title: `Sign-in code ${created.code.name} created`,
                warning: 'Give this code to the person connecting now. It works once, is not stored anywhere and cannot be shown again.',
                secret: created.secret,
            });
            if (main.isConnected) await reload();
        } catch { /* the server's reason is toasted; the form stays filled */ }
    });
    openDrawer('New sign-in code', 'The code is shown once, right after creation.', form);
}

// One code's drawer: its facts, and Revoke while nobody has redeemed it.
function showCode(main, code, reload) {
    const facts = h('dl', { class: 'facts' },
        fact('Id', h('code', {}, code.id)),
        fact('Actor', h('code', {}, code.actor)),
        fact('Access', pill(code.access, ACCESS_KIND[code.access])),
        fact('Catalogs', catalogsOf(code)),
        fact('Default', code.default_catalog ?? '-'),
        fact('State', pill(code.state, CODE_STATE_KIND[code.state])),
        fact('Created', when(code.created_at)),
        fact('Expires', when(code.expires_at)),
        code.redeemed_at ? fact('Redeemed', when(code.redeemed_at)) : null,
        code.grant_id ? fact('Grant', h('code', {}, code.grant_id)) : null,
        code.revoked_at ? fact('Revoked', when(code.revoked_at)) : null);
    const revocable = code.state === 'live' || code.state === 'expired';
    const revoke = revocable ? h('button', {
        type: 'button', id: 'code-revoke', class: 'danger',
        onclick: async () => {
            const ok = await confirmDialog({
                title: `Revoke sign-in code ${code.name}?`,
                message: 'Nobody can redeem it afterwards. This cannot be undone.',
                action: 'Revoke',
            });
            if (!ok) return;
            try {
                await api.del(`/oauth/codes/${enc(code.id)}`);
                toast(`Revoked sign-in code ${code.name}.`, 'ok');
                if (!main.isConnected) return;
                closeDrawer();
                await reload();
            } catch { /* toasted */ }
        },
    }, 'Revoke') : null;
    openDrawer(code.name, 'Sign-in code', facts,
        code.state === 'redeemed'
            ? h('p', { class: 'muted small' }, 'Redeemed: revoke the grant it became in the key list above.')
            : null,
        h('div', { class: 'drawer-actions' }, revoke));
}
