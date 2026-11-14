// Teams: groups of users that own catalogs. Every user has a personal team
// of its own name; shared teams are made here. Any admin lists them; an
// admin key that reaches every catalog creates, changes and deletes them
// (plan Decision 3), and for any other the controls render disabled.
// The server enforces every rule; this page only explains what cannot work.
// A team can also be suspended here: its catalogs then refuse every
// credential but an admin's, except export, until it is lifted.
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
import { guard, once } from '../core/pending.js';

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
              team.personal_of ? [' ', pill('personal', 'muted')] : null,
              team.suspended_at
                  ? [' ', h('span', { dataset: { test: `team-suspended-${team.name}` } }, pill('suspended', 'error'))]
                  : null) },
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
    // The team drawer shown or loading ({name, ticket, last}), and the
    // count of its reads and of the list's (see openDetail and load).
    let shown = null;
    let reads = 0;
    let lists = 0;
    await load();

    // Reads the list again; a slower older read never draws over a newer one.
    async function load() {
        const mine = ++lists;
        try {
            const { teams } = await api.get('/teams');
            if (mine === lists && main.isConnected) table.redraw(teams);
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

    // After a write on team `name` settles: the list is read again, and
    // the drawer that shows `name` now (if any) is drawn again through its
    // own ticket, even when it is not the drawer the write started in (it
    // was closed and opened again meanwhile); a drawer of another team is
    // left alone. `team` is the server's answer (null after a refusal, so
    // the drawer reads the team again), and `refusal` the refused write's
    // reason, which the drawer shows until the next write.
    async function changed(name, team, { message = null, refusal = null } = {}) {
        if (message) toast(message, 'ok');
        if (main.isConnected) await load();
        const view = shown;
        if (view?.name === name && view.ticket.current()) {
            await openDetail(name, view.ticket, { known: team, refusal, last: view.last });
        }
    }

    // The drawer: members with their roles, adding one, the suspension, and
    // Delete, disabled with the reason when the server would refuse it.
    // `ticket` is the drawer's claim: a re-render after a write passes the
    // one the drawer was opened with, so once the drawer is replaced or
    // dismissed the late answer is dropped. `known` is the team as a write
    // answered it (no read needed), `refusal` the reason the last write was
    // refused, and `last` what the drawer showed before. The team and the
    // users (for Add member) are settled one by one: a part that cannot be
    // read again is drawn from `last`, marked as possibly out of date, so
    // the refusal still shows and every control works again, while a part
    // that was read, or a write's answer, is never thrown away for it.
    // Every read takes the next number in `reads` and only the newest one
    // draws, so a read that started before a write never draws over the
    // one after it.
    async function openDetail(name, ticket = drawerTicket(main), { known = null, refusal = null, last = null } = {}) {
        const read = ++reads;
        shown = { name, ticket, last };
        // allSettled, unlike all, waits for both and reports each one's
        // value or failure, so one failed read cannot discard the other.
        const [teamRead, usersRead] = await Promise.allSettled([
            known ?? api.get(`/teams/${enc(name)}`), namesOf('users')]);
        const teamStale = teamRead.status === 'rejected';
        const usersStale = usersRead.status === 'rejected';
        // A part that failed with nothing shown before: no drawer to keep
        // (toasted).
        if ((teamStale || usersStale) && !last) return;
        const team = teamStale ? last.team : teamRead.value;
        const users = usersStale ? last.users : usersRead.value;
        if (read !== reads || !ticket.current()) return;
        shown.last = { team, users };
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
                await changed(team.name, answer.team, { message: `${member.user} is now ${role.value} of ${team.name}.` });
            });
            const personal = member.user === team.personal_of;
            const remove = h('button', {
                type: 'button', class: 'text-btn', disabled: !manager || personal,
                title: personal ? 'A personal team always keeps its user.' : null,
                dataset: { test: `member-remove-${member.user}` },
                onclick: async () => {
                    let updated;
                    try { updated = await api.del(`${base}/${enc(member.user)}`); } catch { return; }
                    await changed(team.name, updated, { message: `Removed ${member.user} from ${team.name}.` });
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
            await changed(team.name, answer.team, { message: `Added ${user} to ${team.name}.` });
        });
        const suspension = suspensionSection(team, refusal);
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
            teamStale || usersStale ? h('p', { id: 'team-stale', class: 'error small', role: 'status' },
                teamStale
                    ? 'Could not read this team again, so what is shown may be out of date.'
                    : 'Could not read the users again, so the users to add may be out of date.') : null,
            h('dl', { class: 'facts', id: 'team-detail' },
                h('dt', {}, 'Catalogs'), h('dd', {}, team.catalogs.length ? team.catalogs.join(', ') : '-'),
                h('dt', {}, 'Created'), h('dd', {}, when(team.created_at))),
            h('section', {},
                h('h3', {}, 'Members'),
                members,
                add),
            suspension,
            manager ? null : h('p', { class: 'muted small' }, NEEDS_EVERY_CATALOG),
            h('section', { class: 'danger-zone' },
                h('h3', {}, 'Delete'),
                remove,
                reason ? h('p', { id: 'team-delete-reason', class: 'muted small' }, reason) : null));
    }

    // The drawer's Suspension section. A suspended team's catalogs refuse
    // every credential but an admin's, except export, until it is lifted;
    // changing it needs the same admin as the other team writes. Its
    // controls are guarded by the pending-write key (core/pending.js), so
    // while a suspend or lift runs they stay disabled, in a drawer opened
    // again meanwhile too. `refusal` is the last write's refused reason.
    // Every conditional child goes through h(), never the DOM's own append,
    // which prints null as "null" (core/dom.js, issue #302).
    function suspensionSection(team, refusal) {
        const key = `team:${team.name}:suspension`;
        // Sends the write, then draws the drawer again from the answer, or
        // from a fresh read after a refusal (whose reason it then shows).
        const write = (suspension, message) => async () => {
            let answer = null;
            let refused = null;
            try {
                answer = await once(key, () => api.put(`/teams/${enc(team.name)}/suspension`, suspension));
            } catch (error) {
                refused = error.message;   // already toasted by api.js
            }
            // undefined: the same write was already running (once ignored it).
            if (answer === undefined) return;
            await changed(team.name, answer, { message: refused ? null : message, refusal: refused });
        };
        const error = refusal ? h('p', { id: 'team-suspension-error', class: 'error small', role: 'alert' }, refusal) : null;
        const heading = h('h3', {}, 'Suspension');
        if (team.suspended_at) {
            const lift = guard(h('button', { type: 'button', id: 'team-unsuspend' }, 'Lift suspension'), key, manager);
            lift.addEventListener('click',
                write({ suspended: false }, `Lifted the suspension of team ${team.name}.`));
            return h('section', { id: 'team-suspension' }, heading,
                h('p', {}, `Suspended since ${when(team.suspended_at)}. Its catalogs refuse everyone but admins, except export.`),
                team.suspension_notice
                    ? h('p', { class: 'muted', id: 'team-suspension-notice' }, team.suspension_notice)
                    : null,
                lift,
                error);
        }
        const notice = guard(h('textarea', { name: 'notice', maxlength: 2000, rows: 3 }), key, manager);
        const submit = guard(h('button', { type: 'submit', id: 'team-suspend', class: 'danger' }, 'Suspend'), key, manager);
        const form = h('form', { id: 'team-suspend-form' },
            h('label', {}, 'Notice for its members (optional)', notice),
            submit);
        form.addEventListener('submit', async event => {
            event.preventDefault();
            const ok = await confirmDialog({
                title: `Suspend team ${team.name}?`,
                message: 'Its catalogs stop answering everyone but admins until you lift it. '
                    + 'Members can still export them. Nothing is deleted.',
                action: 'Suspend',
            });
            if (!ok) return;
            await write({ suspended: true, notice: notice.value }, `Suspended team ${team.name}.`)();
        });
        return h('section', { id: 'team-suspension' }, heading,
            h('p', { class: 'muted small' }, 'Active. Suspending it makes its catalogs refuse everyone but admins, except export.'),
            form,
            error);
    }
}
