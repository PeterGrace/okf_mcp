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
import { guard, once } from '../core/pending.js';
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
    // The filter box narrows the rows already loaded (name, display name or
    // email, any letter case); it never reads from the server. The table
    // keeps the filter and applies it on every redraw, so a list that
    // answers after the text changed is drawn through the newest filter.
    const filter = h('input', { type: 'search', id: 'users-filter', placeholder: 'Filter by name or email',
                                'aria-label': 'Filter users', autocomplete: 'off' });
    const table = listTable(columns, [], {
        onRow: user => openDetail(user.name),
        empty: () => (filter.value.trim() ? 'No user matches the filter.' : 'No users yet.'),
    });
    table.id = 'users-table';
    filter.addEventListener('input', () => {
        const text = filter.value.trim().toLowerCase();
        table.setFilter(text ? user => [user.name, user.display_name, user.email ?? '']
            .some(value => value.toLowerCase().includes(text)) : null);
    });
    // fill(), not native append: the note is null for a manager (issue #302).
    fill(main,
        h('header', { class: 'page-head' },
            h('div', {},
                h('h1', {}, 'Users'),
                h('p', { class: 'muted' }, 'The people keys, grants and sign-in codes belong to. ',
                    h('a', { href: '#/users/unassigned', id: 'unassigned-link' }, 'What is unassigned'))),
            h('div', { class: 'actions' },
                filter,
                h('button', { type: 'button', id: 'user-new', disabled: !manager, onclick: openCreate }, 'New user'))),
        table,
        manager ? null : h('p', { class: 'muted small' }, NEEDS_EVERY_CATALOG));
    await load();

    // The user drawer shown or loading ({name, ticket, last}), and the count
    // of its reads (see openDetail).
    let shown = null;
    let reads = 0;

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
    // Delete, disabled with the reason while its personal team owns a
    // catalog. Deleting takes the user's keys, grants and sign-in codes with
    // it (#379); the server's other refusals (the last owner of a shared
    // team, the last admin user) are kept in the drawer like any action's.
    // `ticket` is the drawer's claim: a re-render after an action passes
    // the one the drawer was opened with, so once the drawer is replaced or
    // dismissed the late answer is dropped. `refusal` is the reason that
    // action was refused (shown until the next action), and `last` the
    // answers the drawer showed before it: when reading the user again
    // fails, the drawer is drawn from them, marked stale, so the refusal
    // still shows and every control can be used again.
    //
    // `shown` is the drawer being shown (or loaded) and whose it is. A write
    // re-renders the drawer that shows its user when it settles, through
    // that drawer's own ticket, even when it is not the drawer the write
    // started in (the user's drawer was closed and opened again meanwhile);
    // a drawer of another user is left alone. Every read takes the next
    // number in `reads`, and only the newest one draws, so a read that
    // started before the write never draws over the one after it.
    async function openDetail(name, ticket = drawerTicket(main), { refusal = null, last = null } = {}) {
        const read = ++reads;
        shown = { name, ticket, last };
        let loaded;
        try {
            const [detail, personal, account] = await Promise.all([
                api.get(`/users/${enc(name)}`), api.get(`/teams/${enc(name)}`), loadAccount(name)]);
            loaded = { detail, personal, account, stale: false };
        } catch {
            if (!last) return;   // the first load: no drawer to keep (toasted)
            loaded = { ...last, stale: true };
        }
        if (read !== reads || !ticket.current()) return;
        const { detail, personal, account, stale } = loaded;
        shown.last = { detail, personal, account };
        const { user, keys, codes } = detail;
        const reason = deleteReason(personal);
        const changed = async refused => {
            // An admin flag or session change of the signed-in user is
            // shell state (the Administer group, the session itself).
            if (name === store.session?.user) await refreshIdentity().catch(() => {});
            if (main.isConnected) await load();
            const view = shown;
            if (view?.name === name && view.ticket.current()) {
                await openDetail(name, view.ticket, { refusal: refused, last: view.last });
            }
        };
        // A pending write keyed by the user (core/pending.js), so a drawer
        // drawn again while it runs (closed and reopened, or redrawn by
        // another action) shows Delete disabled and cannot send it twice.
        const deleting = `user:${name}:delete`;
        const remove = guard(h('button', {
            type: 'button', id: 'user-delete', class: 'danger',
            onclick: async () => {
                const self = name === store.whoami?.user
                    ? ' This is the user you are signed in as: your own access goes with it.' : '';
                const ok = await confirmDialog({
                    title: `Delete user ${name}?`,
                    message: `Its keys, grants and sign-in codes (revoked ones too), console sessions and `
                        + `personal team ${name} are deleted with it, and its open changesets are discarded. `
                        + `Catalogs and their history stay. This cannot be undone.${self}`,
                    action: 'Delete',
                });
                if (ok) await once(deleting, () => removeUser(name));
            },
        }, 'Delete user'), deleting, manager && !reason);
        ticket.open(name, user.display_name || 'User',
            h('div', { id: 'user-detail' },
                stale ? h('p', { id: 'user-stale', class: 'error small', role: 'status' },
                    'Could not read this user again, so what is shown may be out of date.') : null,
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

    // Deletes user `name`. Once it settles, the drawer that shows that user
    // now (not necessarily the one Delete was clicked in) closes, or, when
    // the server refused, is drawn again with the reason kept in it.
    async function removeUser(name) {
        try {
            await api.del(`/users/${enc(name)}`);
        } catch (error) {
            // Already toasted by api.js; the drawer keeps the reason. The
            // redraw starts before any other await, so it takes its read
            // number as the write settles and a later read still wins.
            const view = shown;
            const redraw = view?.name === name && view.ticket.current()
                ? openDetail(name, view.ticket, { refusal: error.message, last: view.last }) : null;
            await Promise.allSettled([redraw, main.isConnected ? load() : null]);
            return;
        }
        toast(`Deleted user ${name}.`, 'ok');
        const view = shown;
        if (view?.name === name && view.ticket.current()) view.ticket.close();
        if (main.isConnected) await load();
    }
}

// Why the server would refuse to delete `user` (whose personal team is
// `personal`), as far as the drawer can tell, or '' when it would not. Its
// keys, grants, sign-in codes and memberships go with it; the last owner of
// a shared team and the last admin user are the server's to refuse.
function deleteReason(personal) {
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
