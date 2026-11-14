// Catalogs: the catalogs this key reaches, with their owner teams. Admins
// edit descriptions and delete; an admin key that reaches every catalog
// also creates them and changes their owner (plan Decision 3).
import { $, h, fill, pill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../core/toast.js';
import { drawerTicket } from '../core/drawer.js';
import { ago, count } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { confirmDialog } from '../components/confirm.js';
import { namesOf, picker, assignControl } from '../components/picker.js';
import { refreshIdentity, refreshUnassigned } from '../core/shell.js';
import { guard, once, recheck } from '../core/pending.js';

export async function render(main) {
    const admin = store.can('admin');
    const creator = admin && store.whoami.all_catalogs;
    const columns = [
        { key: 'name', label: 'Name', sort: row => row.name,
          render: row => h('code', { dataset: { catalog: row.name } }, row.name,
              // `suspended` is {team, notice} while its owner team is suspended.
              row.suspended
                  ? [' ', h('span', { dataset: { test: `catalog-suspended-${row.name}` },
                                     title: row.suspended.notice ?? `Team ${row.suspended.team} is suspended.` },
                            pill('suspended', 'error'))]
                  : null) },
        { key: 'description', label: 'Description',
          render: row => row.description || h('span', { class: 'muted' }, '-') },
        { key: 'owner_team', label: 'Owner', sort: row => row.owner_team ?? '',
          render: row => h('span', { dataset: { test: `catalog-owner-${row.name}` },
                                     class: row.owner_team ? 'mono' : 'muted' }, row.owner_team ?? 'unassigned') },
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
    // rebuilt first, before the table. The drawer is closed only while
    // `ticket` (the one it was opened with) is current: once replaced or
    // dismissed it is another drawer, or none.
    async function settled(ticket) {
        await refreshIdentity().catch(() => {});
        refreshUnassigned();   // a deleted catalog may have been unassigned
        ticket.close();
        if (main.isConnected) await load();
    }

    async function openCreate() {
        const ticket = drawerTicket(main);
        let teams;
        try { teams = await namesOf('teams'); } catch { return; }
        if (!ticket.current()) return;
        const form = h('form', { id: 'catalog-create-form' },
            h('label', {}, 'Name',
                h('input', { name: 'name', required: true, pattern: '[a-z0-9-]+', autocomplete: 'off',
                             placeholder: 'lowercase letters, digits and -' })),
            h('label', {}, 'Description', h('textarea', { name: 'description', rows: '3' })),
            h('label', {}, 'Owner team', picker({ name: 'owner', required: true }, teams)),
            h('button', { type: 'submit' }, 'Create catalog'));
        form.addEventListener('submit', async event => {
            event.preventDefault();
            const data = new FormData(form);
            const name = String(data.get('name') ?? '').trim();
            try {
                await api.post('/catalogs', { name, description: String(data.get('description') ?? ''),
                                              owner: String(data.get('owner') ?? '') });
            } catch { return; /* toasted: invalid or taken name, or unknown owner */ }
            toast(`Created catalog ${name}.`, 'ok');
            await settled(ticket);
        });
        ticket.open('New catalog', 'An empty catalog; import a .tar.gz into it next.', form);
    }

    async function openEdit(row) {
        const ticket = drawerTicket(main);
        let teams;
        try { teams = await namesOf('teams'); } catch { return; }
        if (!ticket.current()) return;
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
            ticket.close();
            if (main.isConnected) await load();
        });
        const owner = h('section', {},
            h('h3', {}, 'Owner'),
            assignControl({
                names: teams, current: row.owner_team, label: row.owner_team ? 'Change owner' : 'Assign',
                allowed: creator, ids: { select: 'catalog-owner-select', button: 'catalog-owner-save' },
                assign: async team => {
                    try {
                        await api.put(`/catalogs/${enc(row.name)}/owner`, { team });
                    } catch { return; /* toasted: an unknown team */ }
                    toast(`${row.name} is now owned by ${team}.`, 'ok');
                    refreshUnassigned();
                    ticket.close();
                    if (main.isConnected) await load();
                },
            }),
            creator ? null : h('p', { class: 'muted small' }, 'Changing the owner needs an admin key that reaches every catalog.'));
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
            await settled(ticket);
        });
        const members = memberAccess(row, creator, ticket);
        ticket.open(row.name, `generation ${row.generation} - ${count(row.concepts, 'concept')}, ${count(row.files, 'file')}`,
            edit,
            owner,
            members.section,
            h('section', { class: 'danger-zone' },
                h('h3', {}, 'Delete'),
                h('label', {}, force, ' Also discard unpublished changes (force)'),
                remove));
        await members.refresh();
    }

    // fill(), not native append: the note may be null (issue #302).
    fill(main,
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

// The member-access section the catalog drawer shows (or last showed):
// {catalog, ticket, refresh, report}. See memberAccess.
let shownMemberAccess = null;

// The drawer's "Team member access": what members of the catalog's owner
// team reach through their accounts (the ceiling's level for it; keys and
// grants they create are capped at it). It is read from GET
// /catalogs/{catalog}/member-access when the drawer opens and again after
// every save, refused or not, so it always shows the server's value. A
// refusal's reason stays in the section until the next save. A read that
// fails keeps the last value, marked as possibly out of date, and leaves
// the controls usable, so the save can be tried again. Changing it
// needs `allowed` (an admin that reaches every catalog: the server's one
// check is `may_set_member_access`). `ticket` is the drawer's claim: an
// answer that arrives after the drawer was replaced or dismissed is
// dropped. Returns the section and `refresh()`, which the caller runs once
// the drawer is open.
//
// A save that settles updates the section the drawer shows now if it is
// this catalog's, through that section's own ticket, even when the drawer
// was closed and opened again while the save ran (`shownMemberAccess`); a
// drawer of another catalog is left alone. Each section draws only its
// newest read, so a read that started before the save never draws over
// the one after it.
function memberAccess(row, allowed, ticket) {
    const path = `/catalogs/${enc(row.name)}/member-access`;
    // The pending-write key (core/pending.js): a save in flight keeps these
    // controls disabled, and those of a drawer reopened meanwhile too.
    const key = `catalog:${row.name}:member-access`;
    // Whether a read has finished, the server's value as last read (null
    // until a read succeeds), and whether the latest read failed.
    let loaded = false;
    let value = null;
    let stale = false;
    const select = guard(h('select', { id: 'catalog-member-access-select', 'aria-label': 'Team member access' },
        ['none', 'read', 'write'].map(option => h('option', { value: option }, option))), key, () => allowed && loaded);
    const current = h('span', { id: 'catalog-member-access-current', class: 'mono' }, '...');
    // Save is offered when the choice differs from the server's value, or
    // always while that value is not known to be current.
    const save = guard(h('button', { type: 'button', id: 'catalog-member-access-save' }, 'Save'), key,
        () => allowed && loaded && (stale || select.value !== value));
    const refusal = h('p', { id: 'catalog-member-access-error', class: 'error small', role: 'alert', hidden: true });
    const sync = () => { recheck(select); recheck(save); };
    select.addEventListener('change', sync);

    let reads = 0;
    async function refresh() {
        const mine = ++reads;
        let answer = null;
        try { answer = await api.get(path); } catch { /* toasted */ }
        if (mine !== reads || !ticket.current()) return;
        loaded = true;
        stale = answer === null;
        if (!stale) {
            value = answer.member_access;
            select.value = value;
        }
        current.textContent = `${value ?? 'unknown'}${stale ? ' (could not be read again, may be out of date)' : ''}`;
        sync();
    }

    // Shows the reason a save was refused, or clears it (null).
    const report = reason => {
        refusal.textContent = reason ?? '';
        refusal.hidden = reason === null;
    };
    const self = { catalog: row.name, ticket, refresh, report };
    shownMemberAccess = self;

    save.addEventListener('click', () => once(key, async () => {
        let refused = null;
        try {
            const answer = await api.put(path, { access: select.value });
            toast(`Members of ${row.owner_team ?? 'the owner team'} now reach ${answer.name} at ${answer.member_access}.`, 'ok');
        } catch (error) {
            refused = error.message;   // already toasted
        }
        const shown = shownMemberAccess;
        const view = shown?.catalog === row.name && shown.ticket.current() ? shown : self;
        view.report(refused);
        await view.refresh();
    }));

    const section = h('section', { id: 'catalog-member-access' },
        h('h3', {}, 'Team member access'),
        h('p', { class: 'muted small' },
            `What members of ${row.owner_team ?? 'its owner team'} reach through their accounts: `
            + 'none, read or write. Their own keys and grants are capped at it.'),
        h('p', { class: 'small' }, 'Now: ', current),
        h('div', { class: 'inline-form' }, select, save),
        refusal,
        allowed ? null : h('p', { class: 'muted small' }, 'Changing it needs an admin key that reaches every catalog.'));
    return { section, refresh };
}
