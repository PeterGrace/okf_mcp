// Users: the people keys, grants and sign-in codes belong to. Any admin
// lists them (to pick one for a key); an admin key that reaches every
// catalog creates and deletes them (plan Decision 3), and for any other the
// controls render disabled. '#/users/unassigned' lists every key, grant,
// code and catalog with no user or owner, each with an inline Assign.
// The server enforces every rule; this page only explains what cannot work.
import { h, pill, fill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../core/toast.js';
import { drawerTicket } from '../core/drawer.js';
import { ago, when } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { confirmDialog } from '../components/confirm.js';
import { assignControl, namesOf } from '../components/picker.js';
import { accountSections, loadAccount } from '../components/user-account.js';
import { canAdministerPeople, keyUserChanged, refreshIdentity, refreshUnassigned } from '../core/shell.js';

const NEEDS_EVERY_CATALOG = 'Changing users needs an admin key that reaches every catalog.';

export async function render(main, rest) {
    if (!store.can('admin')) {
        main.append(h('h1', {}, 'Users'),
            h('p', { class: 'muted', id: 'users-denied' }, 'Managing users needs an admin key.'));
        return;
    }
    if (rest[0] === 'unassigned') {
        await renderUnassigned(main);
        return;
    }
    const manager = canAdministerPeople();
    const columns = [
        { key: 'name', label: 'Name', sort: user => user.name,
          render: user => h('code', { dataset: { test: `user-${user.name}` } }, user.name,
              user.name === store.whoami.user ? [' ', pill('you', 'info')] : null,
              user.is_admin ? [' ', pill('admin', 'warn')] : null) },
        { key: 'display_name', label: 'Display name',
          render: user => user.display_name || h('span', { class: 'muted' }, '-') },
        { key: 'email', label: 'Email', sort: user => user.email ?? '',
          render: user => (user.email ? h('span', { class: 'mono small' }, user.email) : h('span', { class: 'muted' }, '-')) },
        { key: 'credentials', label: 'Credentials', sort: user => user.credentials, class: 'num' },
        { key: 'teams', label: 'Teams', render: user => user.teams.join(', ') },
        { key: 'created_at', label: 'Created', sort: user => user.created_at, render: user => ago(user.created_at) },
    ];
    const table = listTable(columns, [], { onRow: user => openDetail(user.name), empty: 'No users yet.' });
    table.id = 'users-table';
    // fill(), not native append: the note is null for a manager (issue #302).
    fill(main,
        h('header', { class: 'page-head' },
            h('div', {},
                h('h1', {}, 'Users'),
                h('p', { class: 'muted' }, 'The people keys, grants and sign-in codes belong to. ',
                    h('a', { href: '#/users/unassigned', id: 'unassigned-link' }, 'What is unassigned'))),
            h('div', { class: 'actions' },
                h('button', { type: 'button', id: 'user-new', disabled: !manager, onclick: openCreate }, 'New user'))),
        table,
        manager ? null : h('p', { class: 'muted small' }, NEEDS_EVERY_CATALOG));
    await load();

    async function load() {
        try {
            const { users } = await api.get('/users');
            if (main.isConnected) table.redraw(users);
        } catch { /* toasted */ }
    }

    function openCreate() {
        const ticket = drawerTicket(main);
        const form = h('form', { id: 'user-form' },
            h('label', {}, 'Name',
                h('input', { name: 'name', required: true, pattern: '[a-z0-9-]+', autocomplete: 'off',
                             placeholder: 'lowercase letters, digits and -; also its personal team' })),
            h('label', {}, 'Display name', h('input', { name: 'display_name', autocomplete: 'off' })),
            h('button', { type: 'submit' }, 'Create user'));
        form.addEventListener('submit', async event => {
            event.preventDefault();
            const data = new FormData(form);
            const name = String(data.get('name') ?? '').trim();
            try {
                await api.post('/users', { name, display_name: String(data.get('display_name') ?? '').trim() });
            } catch { return; /* toasted: an invalid or taken name */ }
            toast(`Created user ${name} and its personal team.`, 'ok');
            ticket.close();
            if (main.isConnected) await load();
        });
        ticket.open('New user', 'Creates its personal team of the same name.', form);
    }

    // The drawer: the user's teams, the credentials this key manages, the
    // account sections (sign-in, entitlements, sessions, sign-ins), and
    // Delete, disabled with the reason while anything still names the user.
    // `ticket` is the drawer's claim: a re-render after an action passes
    // the one the drawer was opened with, so once the drawer is replaced or
    // dismissed the late answer is dropped. `again` marks such a re-render,
    // and `refusal` is the reason its action was refused (shown until the
    // next action).
    async function openDetail(name, ticket = drawerTicket(main), { again = false, refusal = null } = {}) {
        let detail;
        let personal;
        let account;
        try {
            [detail, personal, account] = await Promise.all([
                api.get(`/users/${enc(name)}`), api.get(`/teams/${enc(name)}`), loadAccount(name)]);
        } catch {
            // A re-render that cannot read the user again closes the drawer
            // rather than leave the old state showing as if current.
            if (again) ticket.close();
            return;
        }
        if (!ticket.current()) return;
        const { user, keys, codes } = detail;
        const reason = deleteReason(user, personal);
        const changed = async refused => {
            // An admin flag or session change of the signed-in user is
            // shell state (the Administer group, the session itself).
            if (name === store.session?.user) await refreshIdentity().catch(() => {});
            if (main.isConnected) await load();
            if (ticket.current()) await openDetail(name, ticket, { again: true, refusal: refused });
        };
        const remove = h('button', {
            type: 'button', id: 'user-delete', class: 'danger', disabled: !manager || !!reason,
            onclick: async () => {
                const ok = await confirmDialog({
                    title: `Delete user ${name}?`,
                    message: `Its personal team ${name} is deleted with it. This cannot be undone.`,
                    action: 'Delete',
                });
                if (!ok) return;
                try { await api.del(`/users/${enc(name)}`); } catch { return; }
                toast(`Deleted user ${name}.`, 'ok');
                ticket.close();
                if (main.isConnected) await load();
            },
        }, 'Delete user');
        ticket.open(name, user.display_name || 'User',
            h('div', { id: 'user-detail' },
                h('p', {}, 'Personal team: ', h('code', {}, personal.name)),
                h('dl', { class: 'facts' },
                    h('dt', {}, 'Teams'), h('dd', {}, user.teams.join(', ')),
                    h('dt', {}, 'Created'), h('dd', {}, when(user.created_at))),
                accountSections({ detail, account, manager, refusal, changed }),
                h('h3', {}, 'Keys and grants'),
                h('ul', { class: 'grant-list', id: 'user-keys' }, keys.length
                    ? keys.map(key => h('li', {}, h('code', {}, key.name), ` ${key.kind}, ${key.access}`,
                        key.revoked_at ? [' ', pill('revoked', 'error')] : null))
                    : h('li', { class: 'muted' }, 'None that this key manages.')),
                h('h3', {}, 'Sign-in codes'),
                h('ul', { class: 'grant-list', id: 'user-codes' }, codes.length
                    ? codes.map(code => h('li', {}, h('code', {}, code.name), ' ', pill(code.state)))
                    : h('li', { class: 'muted' }, 'None that this key manages.')),
                h('section', { class: 'danger-zone' },
                    h('h3', {}, 'Delete'),
                    remove,
                    reason || !manager
                        ? h('p', { id: 'user-delete-reason', class: 'muted small' }, reason || NEEDS_EVERY_CATALOG)
                        : null)));
    }
}

// Why the server would refuse to delete `user` (whose personal team is
// `personal`), or '' when it would not.
function deleteReason(user, personal) {
    const n = user.credentials;
    if (n > 0) {
        return n === 1
            ? '1 key, grant or sign-in code names this user. Reassign it first.'
            : `${n} keys, grants or sign-in codes name this user. Reassign them first.`;
    }
    const others = user.teams.filter(team => team !== personal.name);
    if (others.length) return `This user belongs to other teams: ${others.join(', ')}.`;
    if (personal.catalogs.length) return `Its personal team owns catalogs: ${personal.catalogs.join(', ')}.`;
    return '';
}

// Every unassigned key, grant, sign-in code and catalog, each with a user
// (or team) picker and an Assign button. The listing needs an admin key
// that reaches every catalog, as assigning a catalog's owner does.
async function renderUnassigned(main) {
    main.append(h('header', { class: 'page-head' },
        h('div', {},
            h('h1', {}, 'Unassigned'),
            h('p', { class: 'muted' }, 'What predates users and teams: give each a user, or a catalog its owner team. ',
                h('a', { href: '#/users' }, 'Back to users')))));
    if (!canAdministerPeople()) {
        main.append(h('p', { class: 'muted', id: 'unassigned-denied' },
            'Listing what is unassigned needs an admin key that reaches every catalog.'));
        return;
    }
    const body = h('div', { id: 'unassigned' });
    main.append(body);
    await draw();

    async function draw() {
        let rows;
        let users;
        let teams;
        try {
            [rows, users, teams] = await Promise.all([api.get('/unassigned'), namesOf('users'), namesOf('teams')]);
        } catch { return; }
        if (!main.isConnected) return;
        const items = [
            ...rows.keys.map(key => assignRow(`key-${key.name}`,
                [key.kind === 'oauth' ? 'Grant ' : 'Key ', h('code', {}, key.name), ` (actor ${key.actor})`,
                    key.revoked_at ? [' ', pill('revoked', 'error')] : null],
                users, async user => {
                    await api.put(`/keys/${enc(key.id)}/user`, { user });
                    await keyUserChanged(key.id);
                }, `${key.name} now belongs to`)),
            ...rows.codes.map(code => assignRow(`code-${code.name}`,
                ['Sign-in code ', h('code', {}, code.name), ' ', pill(code.state)],
                users, user => api.put(`/oauth/codes/${enc(code.id)}/user`, { user }), `${code.name} now belongs to`)),
            ...rows.catalogs.map(name => assignRow(`catalog-${name}`,
                ['Catalog ', h('code', {}, name)],
                teams, team => api.put(`/catalogs/${enc(name)}/owner`, { team }), `${name} is now owned by`)),
        ];
        body.replaceChildren(items.length
            ? h('ul', { class: 'grant-list' }, items)
            : h('p', { id: 'unassigned-empty', class: 'muted' }, 'Nothing is unassigned.'));
    }

    function assignRow(test, label, names, assign, done) {
        return h('li', { dataset: { test: `assign-${test}` } }, label,
            assignControl({
                names, label: 'Assign',
                assign: async name => {
                    try { await assign(name); } catch { return; /* toasted */ }
                    toast(`${done} ${name}.`, 'ok');
                    await refreshUnassigned();
                    if (main.isConnected) await draw();
                },
            }));
    }
}
