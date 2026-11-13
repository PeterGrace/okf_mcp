// The account parts of a user's drawer (M12): sign-in (email, admin flag,
// Send setup email), entitlements (catalogs beyond the user's teams'), the
// user's console sessions, and recent sign-ins. Any admin reads them; every
// change needs an admin that reaches every catalog (`manager`), and for any
// other admin the controls render disabled with the reason.
//
// Each action toasts the server's answer, then calls `changed(refusal)`,
// which reads the user again and re-renders the drawer while its ticket is
// current. Every control therefore shows the server's state after the
// action (a refused admin toggle springs back), and a refusal's reason
// stays in the drawer (`#user-refusal`) until the next action. Each action
// is a pending write (core/pending.js) keyed `user:<name>:<what>`, so its
// controls stay disabled until it and that re-render finish, however often
// the drawer is drawn meanwhile, and it never runs twice at once. When that
// read fails, the drawer is re-rendered from the last answer, marked stale
// (`#user-stale`), so the reason still shows and every control works.
import { h, fact } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { toast } from '../core/toast.js';
import { count } from '../core/format.js';
import { guard, once } from '../core/pending.js';
import { confirmDialog } from './confirm.js';
import { picker } from './picker.js';
import { sessionTable, signInTable } from './activity.js';

const LEVELS = ['read', 'write'];
const NEEDS_EVERY_CATALOG = 'Changing how a user signs in needs an admin key that reaches every catalog.';

// What the account sections show beyond GET /users/{name}: the sessions,
// the sign-ins and the catalog names an entitlement may name. Rejects with
// the toasted ApiError on failure.
export async function loadAccount(name) {
    const base = `/users/${enc(name)}`;
    const [{ sessions }, { sign_ins: signIns }, { catalogs }] = await Promise.all([
        api.get(`${base}/sessions`), api.get(`${base}/sign-ins`), api.get('/catalogs')]);
    return { sessions, signIns, catalogs: catalogs.map(row => row.name) };
}

// The sections, in drawer order. `detail` is GET /users/{name}'s answer,
// `account` loadAccount's, `refusal` the reason the previous action was
// refused (or null).
export function accountSections({ detail, account, manager, refusal, changed }) {
    const { user, entitlements } = detail;
    // `what` names an action within this user (`email`, `admin`, ...), and
    // `pending(what)` is its full key, which its controls are guarded by.
    const pending = what => `user:${user.name}:${what}`;
    // Sends `request`, toasts `message(answer)` or keeps the refusal, then
    // re-renders through `changed`.
    const run = async (request, message) => {
        let refused = null;
        try {
            toast(message(await request()), 'ok');
        } catch (error) {
            refused = error.message;   // already toasted by api.js
        }
        await changed(refused);
    };
    const act = (what, request, message) => once(pending(what), () => run(request, message));
    const tools = { act, run, pending, manager };
    return [
        refusal ? h('p', { id: 'user-refusal', class: 'error small', role: 'alert' }, refusal) : null,
        manager ? null : h('p', { id: 'user-account-readonly', class: 'muted small' }, NEEDS_EVERY_CATALOG),
        signInSection(user, tools),
        entitlementSection(user.name, entitlements, account.catalogs, tools),
        sessionSection(user.name, account.sessions, tools),
        h('section', { id: 'user-sign-ins' },
            h('h3', {}, 'Recent sign-ins'),
            signInTable({ id: 'user-sign-in-table', signIns: account.signIns })),
    ];
}

function signInSection(user, { act, pending, manager }) {
    const base = `/users/${enc(user.name)}`;
    const email = guard(h('input', { type: 'email', name: 'email', value: user.email ?? '', autocomplete: 'off',
                                     placeholder: 'name@example.com', 'aria-label': 'Email' }),
        pending('email'), manager);
    const save = guard(h('button', { type: 'submit' }, 'Save email'), pending('email'), manager);
    const clear = user.email ? guard(h('button', {
        type: 'button', id: 'user-email-clear', class: 'text-btn',
        onclick: () => act('email', () => api.put(`${base}/email`, { email: null }),
            () => `Cleared ${user.name}'s email.`),
    }, 'Clear'), pending('email'), manager) : null;
    const form = h('form', { id: 'user-email-form', class: 'inline-form' }, email, save, clear);
    form.addEventListener('submit', event => {
        event.preventDefault();
        const next = email.value.trim();
        act('email', () => api.put(`${base}/email`, { email: next || null }),
            answer => answer.email ? `${answer.name}'s email is now ${answer.email}.` : `Cleared ${answer.name}'s email.`);
    });
    const admin = guard(h('input', { type: 'checkbox', role: 'switch', id: 'user-admin-toggle', checked: user.is_admin }),
        pending('admin'), manager);
    admin.addEventListener('change', () => act('admin', () => api.put(`${base}/admin`, { admin: admin.checked }),
        answer => (answer.admin ? `${answer.name} is now an admin.` : `${answer.name} is no longer an admin.`)));
    const invite = guard(h('button', {
        type: 'button', id: 'user-invite', class: 'secondary',
        onclick: () => act('invite', () => api.post(`${base}/invite`), answer => `Queued a setup email to ${answer.email}.`),
    }, 'Send setup email'), pending('invite'), manager && !!user.email);
    return h('section', { id: 'user-account' },
        h('h3', {}, 'Sign-in'),
        h('dl', { class: 'facts' },
            fact('Email', h('span', { dataset: { test: `user-${user.name}-email` } }, user.email ?? 'none')),
            fact('Password', h('span', { id: 'user-password' }, user.has_password ? 'set' : 'not set'))),
        form,
        h('label', {}, admin, ' Admin: signs in to the full console'),
        h('div', { class: 'drawer-actions' }, invite),
        user.email ? null : h('p', { id: 'user-invite-reason', class: 'muted small' },
            'Set an email first: the setup link is sent there.'));
}

function entitlementSection(name, entitlements, catalogs, { act, pending, manager }) {
    const base = `/users/${enc(name)}/entitlements`;
    const done = answer => {
        const levels = answer.entitlements.map(entry => `${entry.access} on ${entry.catalog}`);
        return `${answer.name} now has ${levels.length ? levels.join(', ') : 'no entitlements'}.`;
    };
    const rows = entitlements.map(entry => {
        // A level change and a removal of one catalog share its key.
        const what = `entitlement:${entry.catalog}`;
        const level = guard(h('select', { 'aria-label': `Access to ${entry.catalog}`,
                                          dataset: { test: `entitlement-level-${entry.catalog}` } },
            LEVELS.map(value => h('option', { value, selected: value === entry.access }, value))), pending(what), manager);
        level.addEventListener('change', () => act(what,
            () => api.put(`${base}/${enc(entry.catalog)}`, { access: level.value }), done));
        const remove = guard(h('button', {
            type: 'button', class: 'text-btn',
            dataset: { test: `entitlement-remove-${entry.catalog}` },
            onclick: () => act(what, () => api.del(`${base}/${enc(entry.catalog)}`), done),
        }, 'Remove'), pending(what), manager);
        return h('li', { dataset: { test: `entitlement-${entry.catalog}` } },
            h('span', { class: 'mono' }, entry.catalog), level, remove);
    });
    const held = new Set(entitlements.map(entry => entry.catalog));
    const addable = catalogs.filter(catalog => !held.has(catalog));
    const catalog = picker({ name: 'catalog', required: true, 'aria-label': 'Catalog to entitle', disabled: !manager }, addable);
    const level = h('select', { name: 'access', 'aria-label': 'Access', disabled: !manager },
        LEVELS.map(value => h('option', { value }, value)));
    const submit = guard(h('button', { type: 'submit' }, 'Entitle'), pending('entitle'), manager);
    const form = h('form', { id: 'entitlement-form', class: 'inline-form' }, catalog, level, submit);
    form.addEventListener('submit', event => {
        event.preventDefault();
        act('entitle', () => api.put(`${base}/${enc(catalog.value)}`, { access: level.value }), done);
    });
    return h('section', { id: 'user-entitlements' },
        h('h3', {}, 'Entitlements'),
        h('p', { class: 'muted small' },
            'Catalogs beyond those of the user\'s teams. Keys and grants the user creates are capped at the '
            + 'ceiling: these, plus the team catalogs at each catalog\'s member access, the higher level winning.'),
        h('ul', { class: 'grant-list', id: 'entitlement-list' },
            rows.length ? rows : h('li', { class: 'muted' }, 'No entitlements.')),
        addable.length ? form : null);
}

function sessionSection(name, sessions, { act, run, pending, manager }) {
    const base = `/users/${enc(name)}/sessions`;
    const revokeAll = sessions.length ? guard(h('button', {
        type: 'button', id: 'user-sessions-revoke-all', class: 'secondary',
        onclick: async () => {
            const ok = await confirmDialog({
                title: `Sign ${name} out everywhere?`,
                message: `Ends ${count(sessions.length, 'console session')}. Keys and OAuth grants keep working.`,
                action: 'Sign out everywhere',
            });
            if (ok) {
                await act('sessions', () => api.del(base),
                    answer => `Signed ${answer.name} out of ${count(answer.revoked, 'console session')}.`);
            }
        },
    }, 'Sign out everywhere'), pending('sessions'), manager) : null;
    return h('section', { id: 'user-sessions' },
        h('h3', {}, 'Console sessions'),
        sessionTable({
            id: 'user-session-table', prefix: 'user-session', sessions, allowed: manager,
            scope: pending('session'),
            // sessionTable keys this write itself (`<scope>:<id_prefix>`).
            revoke: row => run(() => api.del(`${base}/${enc(row.id_prefix)}`),
                () => `Signed ${name} out of that session.`),
        }),
        revokeAll);
}
