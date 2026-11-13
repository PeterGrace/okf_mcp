// Teams: groups of users that own catalogs. Every user has a personal team
// of its own name; shared teams are made here. Any admin lists them; an
// admin key that reaches every catalog creates, changes and deletes them
// (plan Decision 3), and for any other the controls render disabled.
// The server enforces every rule; this page only explains what cannot work.
import { h, pill, fill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../core/toast.js';
import { drawerTicket } from '../core/drawer.js';
import { ago, count, when } from '../core/format.js';
import { listTable } from '../components/list-table.js';
import { confirmDialog } from '../components/confirm.js';
import { namesOf, picker } from '../components/picker.js';
import { canAdministerPeople } from '../core/shell.js';

const ROLES = ['member', 'owner'];
const NEEDS_EVERY_CATALOG = 'Changing teams needs an admin key that reaches every catalog.';

function roleSelect(attrs, selected) {
    return h('select', attrs, ROLES.map(role => h('option', { value: role, selected: role === selected }, role)));
}

export async function render(main) {
    if (!store.can('admin')) {
        main.append(h('h1', {}, 'Teams'),
            h('p', { class: 'muted', id: 'teams-denied' }, 'Managing teams needs an admin key.'));
        return;
    }
    const manager = canAdministerPeople();
    const columns = [
        { key: 'name', label: 'Name', sort: team => team.name,
          render: team => h('code', { dataset: { test: `team-${team.name}` } }, team.name,
              team.personal_of ? [' ', pill('personal', 'muted')] : null) },
        { key: 'display_name', label: 'Display name',
          render: team => team.display_name || h('span', { class: 'muted' }, '-') },
        { key: 'members', label: 'Members', sort: team => team.members.length,
          render: team => count(team.members.length, 'member') },
        { key: 'catalogs', label: 'Catalogs',
          render: team => team.catalogs.length ? team.catalogs.join(', ') : h('span', { class: 'muted' }, '-') },
        { key: 'created_at', label: 'Created', sort: team => team.created_at, render: team => ago(team.created_at) },
    ];
    const table = listTable(columns, [], { onRow: team => openDetail(team.name), empty: 'No teams yet.' });
    table.id = 'teams-table';
    // fill(), not native append: the note is null for a manager (issue #302).
    fill(main,
        h('header', { class: 'page-head' },
            h('div', {},
                h('h1', {}, 'Teams'),
                h('p', { class: 'muted' }, 'Groups of users that own catalogs. Each user also has a personal team.')),
            h('div', { class: 'actions' },
                h('button', { type: 'button', id: 'team-new', disabled: !manager, onclick: openCreate }, 'New team'))),
        table,
        manager ? null : h('p', { class: 'muted small' }, NEEDS_EVERY_CATALOG));
    await load();

    async function load() {
        try {
            const { teams } = await api.get('/teams');
            if (main.isConnected) table.redraw(teams);
        } catch { /* toasted */ }
    }

    async function openCreate() {
        const ticket = drawerTicket(main);
        let users;
        try { users = await namesOf('users'); } catch { return; }
        if (!ticket.current()) return;
        const form = h('form', { id: 'team-form' },
            h('label', {}, 'Name',
                h('input', { name: 'name', required: true, pattern: '[a-z0-9-]+', autocomplete: 'off',
                             placeholder: 'lowercase letters, digits and -' })),
            h('label', {}, 'Display name', h('input', { name: 'display_name', autocomplete: 'off' })),
            h('label', {}, 'Owner', picker({ name: 'owner', required: true }, users)),
            h('button', { type: 'submit' }, 'Create team'));
        form.addEventListener('submit', async event => {
            event.preventDefault();
            const data = new FormData(form);
            const name = String(data.get('name') ?? '').trim();
            try {
                await api.post('/teams', {
                    name,
                    display_name: String(data.get('display_name') ?? '').trim(),
                    owner: String(data.get('owner') ?? ''),
                });
            } catch { return; /* toasted: an invalid or taken name, or an unknown owner */ }
            toast(`Created team ${name}.`, 'ok');
            ticket.close();
            if (main.isConnected) await load();
        });
        ticket.open('New team', 'A shared team; its owner is its first member.', form);
    }

    // After a membership change: the list shows its new member count, and
    // the drawer, while `ticket` (the one it was opened with) is current,
    // shows `team` as it now is.
    async function changed(team, message, ticket) {
        toast(message, 'ok');
        if (main.isConnected) await load();
        if (ticket.current()) await openDetail(team.name, team, ticket);
    }

    // The drawer: members with their roles, adding one, and Delete,
    // disabled with the reason when the server would refuse it. A re-render
    // passes the drawer's `ticket`, so it is dropped once the drawer is
    // replaced or dismissed.
    async function openDetail(name, known = null, ticket = drawerTicket(main)) {
        let team = known;
        let users;
        try {
            [team, users] = await Promise.all([team ?? api.get(`/teams/${enc(name)}`), namesOf('users')]);
        } catch { return; }
        if (!ticket.current()) return;
        const base = `/teams/${enc(team.name)}/members`;
        const members = h('ul', { class: 'grant-list', id: 'team-members' }, team.members.map(member => {
            const role = roleSelect({ 'aria-label': `Role of ${member.user}`, disabled: !manager,
                                      dataset: { test: `member-role-${member.user}` } }, member.role);
            role.addEventListener('change', async () => {
                let answer;
                try { answer = await api.put(`${base}/${enc(member.user)}`, { role: role.value }); } catch {
                    role.value = member.role;   // refused (the last owner): show the role it kept
                    return;
                }
                await changed(answer.team, `${member.user} is now ${role.value} of ${team.name}.`, ticket);
            });
            const personal = member.user === team.personal_of;
            const remove = h('button', {
                type: 'button', class: 'text-btn', disabled: !manager || personal,
                title: personal ? 'A personal team always keeps its user.' : null,
                dataset: { test: `member-remove-${member.user}` },
                onclick: async () => {
                    let updated;
                    try { updated = await api.del(`${base}/${enc(member.user)}`); } catch { return; }
                    await changed(updated, `Removed ${member.user} from ${team.name}.`, ticket);
                },
            }, 'Remove');
            return h('li', {}, h('span', { class: 'mono' }, member.user), role, remove);
        }));
        const candidates = users.filter(user => !team.members.some(member => member.user === user));
        const add = h('form', { id: 'member-form', class: 'inline-form' },
            picker({ name: 'user', required: true, 'aria-label': 'User to add', disabled: !manager }, candidates),
            roleSelect({ name: 'role', 'aria-label': 'Role', disabled: !manager }, 'member'),
            h('button', { type: 'submit', disabled: !manager || !candidates.length }, 'Add member'));
        add.addEventListener('submit', async event => {
            event.preventDefault();
            const data = new FormData(add);
            const user = String(data.get('user') ?? '');
            let answer;
            try { answer = await api.put(`${base}/${enc(user)}`, { role: String(data.get('role') ?? 'member') }); } catch {
                return;
            }
            await changed(answer.team, `Added ${user} to ${team.name}.`, ticket);
        });
        const reason = team.personal_of
            ? `This is ${team.personal_of}'s personal team: it is deleted with its user.`
            : team.catalogs.length ? `It owns catalogs: ${team.catalogs.join(', ')}. Give them another owner first.` : '';
        const remove = h('button', {
            type: 'button', id: 'team-delete', class: 'danger', disabled: !manager || !!reason,
            onclick: async () => {
                const ok = await confirmDialog({
                    title: `Delete team ${team.name}?`,
                    message: 'Its memberships are deleted with it. This cannot be undone.',
                    action: 'Delete',
                });
                if (!ok) return;
                try { await api.del(`/teams/${enc(team.name)}`); } catch { return; }
                toast(`Deleted team ${team.name}.`, 'ok');
                ticket.close();
                if (main.isConnected) await load();
            },
        }, 'Delete team');
        ticket.open(team.name, team.display_name || (team.personal_of ? 'Personal team' : 'Team'),
            h('dl', { class: 'facts', id: 'team-detail' },
                h('dt', {}, 'Catalogs'), h('dd', {}, team.catalogs.length ? team.catalogs.join(', ') : '-'),
                h('dt', {}, 'Created'), h('dd', {}, when(team.created_at))),
            h('section', {},
                h('h3', {}, 'Members'),
                members,
                add),
            manager ? null : h('p', { class: 'muted small' }, NEEDS_EVERY_CATALOG),
            h('section', { class: 'danger-zone' },
                h('h3', {}, 'Delete'),
                remove,
                reason ? h('p', { id: 'team-delete-reason', class: 'muted small' }, reason) : null));
    }
}
