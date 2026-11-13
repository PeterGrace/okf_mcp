// Account: how the signed-in person signs in, and where. Profile and
// ceiling (read-only, from /me on every load, never the sign-in snapshot),
// password, passkeys (add, rename, remove), linked sign-in accounts (link,
// unlink), console sessions (sign any one out, this browser's included)
// and recent sign-ins. Each section loads and redraws on its own, only
// while the page is shown. The server refuses removing the last sign-in
// method (409, toasted with its reason). Changing the password, adding a
// passkey or linking an account signs the other sessions out, unless
// "Keep me signed in elsewhere" is ticked.
import { h, fill, fact } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { toast } from '../core/toast.js';
import { ago, when } from '../core/format.js';
import { freshAccount, requireSession, signedOut } from '../core/shell.js';
import { listTable } from '../components/list-table.js';
import { confirmDialog } from '../components/confirm.js';
import { accessPill } from '../components/levels.js';
import { sessionTable, signInTable } from '../components/activity.js';
import { createCredential } from '../signin/webauthn.js';

export async function render(main) {
    main.append(h('header', { class: 'page-head' },
        h('div', {},
            h('h1', {}, 'Account'),
            h('p', { class: 'muted' }, 'How you sign in, where you are signed in, and what your account may reach.'))));
    if (!requireSession(main, 'account-denied')) return;
    const profile = section('account-profile', 'Profile');
    const ceiling = section('account-ceiling', 'Your ceiling',
        'The most a key or OAuth grant you create can reach: the catalogs of your teams, at each catalog\'s '
        + 'member access, and the catalogs an admin entitled you to; where both apply, the higher level. '
        + 'An admin changes it.');
    const password = section('account-password', 'Password');
    const passkeys = section('account-passkeys', 'Passkeys', 'Sign in with your device instead of a password.');
    const identities = section('account-identities', 'Linked accounts', 'Sign in through another identity provider.');
    const sessions = section('account-sessions', 'Where you are signed in');
    const signIns = section('account-sign-ins', 'Recent sign-ins');
    main.append(profile.el, ceiling.el, password.el, passkeys.el, identities.el, sessions.el, signIns.el);

    async function drawAccount() {
        let account;
        try { account = await freshAccount(); } catch { return; /* toasted */ }
        if (!main.isConnected) return;
        fill(profile.body, h('dl', { class: 'facts', id: 'account-facts' },
            fact('Name', h('code', {}, account.user)),
            fact('Display name', account.display_name || '-'),
            fact('Email', account.email ?? 'none'),
            fact('Teams', account.teams.length ? account.teams.join(', ') : '-'),
            fact('Admin', account.admin ? 'yes: you sign in to the full console' : 'no')));
        const table = listTable([
            { key: 'catalog', label: 'Catalog', sort: row => row.catalog,
              render: row => h('code', { dataset: { test: `ceiling-${row.catalog}` } }, row.catalog) },
            { key: 'access', label: 'Access', render: row => accessPill(row.access) },
        ], account.ceiling, { empty: 'Your account reaches no catalog yet. Ask an admin.' });
        table.id = 'ceiling-table';
        fill(ceiling.body, table);
        fill(password.body, passwordForm(account));
    }

    function passwordForm(account) {
        const current = account.has_password
            ? h('input', { type: 'password', name: 'current', required: true, autocomplete: 'current-password' })
            : null;
        const form = h('form', { id: 'password-form' },
            account.password_sign_in ? null : h('p', { class: 'muted small', id: 'password-sign-in-off' },
                'Password sign-in is turned off on this server: a password works once an admin turns it back on.'),
            current
                ? h('label', {}, 'Current password', current)
                : h('p', { class: 'muted small' }, 'You have no password yet. Set one to sign in with your email.'),
            h('label', {}, 'New password',
                h('input', { type: 'password', name: 'password', required: true, minlength: '12', autocomplete: 'new-password' })),
            h('label', {}, 'Repeat the new password',
                h('input', { type: 'password', name: 'confirm', required: true, minlength: '12', autocomplete: 'new-password' })),
            h('p', { class: 'muted small' }, 'At least 12 characters, and not a common password.'),
            keepSessionsBox('password-keep-sessions'),
            h('button', { type: 'submit' }, account.has_password ? 'Change password' : 'Set password'));
        form.addEventListener('submit', async event => {
            event.preventDefault();
            const data = new FormData(form);
            const next = String(data.get('password') ?? '');
            if (next !== String(data.get('confirm') ?? '')) {
                toast('The two new passwords differ.', 'error');
                return;
            }
            try {
                // A wrong current password is a 403, never a 401, so the
                // console stays signed in; the policy's refusal is a 422.
                await api.put('/me/password', {
                    current_password: current ? current.value : null,
                    new_password: next,
                    keep_sessions: data.get('keep_sessions') === 'on',
                }, { quiet: true });
            } catch (error) {
                if (error.status === 401) return; // signed out by the 401 handler
                toast(`The password was not changed: ${reason(error)}.`, 'error');
                if (current && error.status === 403) {
                    current.value = '';
                    current.focus();
                }
                return;
            }
            toast(account.has_password ? 'Password changed.' : 'Password set.', 'ok');
            if (main.isConnected) await Promise.all([drawAccount(), drawSessions()]);
        });
        return form;
    }

    async function drawPasskeys() {
        let list;
        try { ({ passkeys: list } = await api.get('/me/passkeys')); } catch { return; }
        if (!main.isConnected) return;
        const name = h('input', { id: 'passkey-name', 'aria-label': 'A name for the new passkey',
                                  placeholder: 'a name, such as laptop', autocomplete: 'off' });
        const keep = keepSessionsBox('passkey-keep-sessions');
        const add = h('button', { type: 'button', id: 'passkey-add' }, 'Add a passkey');
        add.addEventListener('click', async () => {
            add.disabled = true;
            try {
                await addPasskey(name.value.trim() || 'passkey', keep.querySelector('input').checked);
            } finally {
                add.disabled = false;
            }
        });
        fill(passkeys.body,
            h('ul', { class: 'grant-list', id: 'passkey-list' },
                list.length ? list.map(passkeyRow) : h('li', { class: 'muted' }, 'No passkeys yet.')),
            h('div', { class: 'inline-form' }, name, add),
            keep);
    }

    function passkeyRow(passkey) {
        const base = `/me/passkeys/${enc(passkey.id)}`;
        const label = h('input', { value: passkey.name, autocomplete: 'off', 'aria-label': `New name for ${passkey.name}` });
        const rename = async () => {
            const next = label.value.trim();
            if (!next || next === passkey.name) return;
            try { await api.patch(base, { name: next }); } catch { return; /* toasted */ }
            toast(`Renamed passkey ${passkey.name} to ${next}.`, 'ok');
            if (main.isConnected) await drawPasskeys();
        };
        const remove = async () => {
            const ok = await confirmDialog({
                title: `Remove passkey ${passkey.name}?`,
                message: 'You can no longer sign in with it. Remove it from the device too.',
                action: 'Remove',
            });
            if (!ok) return;
            try { await api.del(base); } catch { return; /* toasted: 409 says it is the last sign-in method */ }
            toast(`Removed passkey ${passkey.name}.`, 'ok');
            if (main.isConnected) await drawPasskeys();
        };
        return h('li', { dataset: { test: `passkey-${passkey.name}` } },
            label,
            h('button', { type: 'button', class: 'secondary', dataset: { test: `passkey-rename-${passkey.name}` }, onclick: rename }, 'Rename'),
            h('span', { class: 'muted small' },
                `added ${when(passkey.created_at)}, last used ${passkey.last_used_at ? ago(passkey.last_used_at) : 'never'}`),
            h('button', { type: 'button', class: 'text-btn', dataset: { test: `passkey-remove-${passkey.name}` }, onclick: remove }, 'Remove'));
    }

    // Registration (Task 8): the server's challenge (start takes the name
    // and the keep-sessions choice, and sets the ceremony cookie), the
    // browser's prompt, then the credential back to the server. A cancelled
    // or timed-out prompt is a calm note, never an error or a script error.
    async function addPasskey(name, keep) {
        let challenge;
        try {
            challenge = await api.post('/me/passkeys/register/start', { name, keep_sessions: keep });
        } catch { return; /* toasted */ }
        let credential;
        try {
            credential = await createCredential(challenge);
        } catch (error) {
            if (error?.name === 'NotAllowedError') {
                toast('No passkey was added: the browser prompt was cancelled or timed out.', 'info');
            } else {
                toast(`No passkey was added: ${error?.message ?? error}`, 'error');
            }
            return;
        }
        try {
            await api.post('/me/passkeys/register/finish', credential, { quiet: true });
        } catch (error) {
            // 403: another session started this ceremony; 409: a passkey
            // already registered; 410: the ceremony expired; 422: a
            // credential the server could not verify. Only a 401 (this
            // session ended) signs the console out.
            if (error.status !== 401) toast(`No passkey was added: ${reason(error)}.`, 'error');
            return;
        }
        toast(`Added passkey ${name}.`, 'ok');
        if (main.isConnected) await Promise.all([drawPasskeys(), drawSessions()]);
    }

    async function drawIdentities() {
        let linked;
        let providers;
        try { ({ identities: linked, providers } = await api.get('/me/identities')); } catch { return; }
        if (!main.isConnected) return;
        const label = id => providers.find(provider => provider.id === id)?.label ?? id;
        const held = new Set(linked.map(identity => identity.provider));
        const keep = keepSessionsBox('identity-keep-sessions');
        fill(identities.body,
            h('ul', { class: 'grant-list', id: 'identity-list' }, linked.length
                ? linked.map(identity => h('li', { dataset: { test: `identity-${identity.provider}` } },
                    h('strong', {}, label(identity.provider)), ' ',
                    h('span', { class: 'mono small' }, identity.email_at_link),
                    h('span', { class: 'muted small' }, ` linked ${when(identity.created_at)}`),
                    h('button', {
                        type: 'button', class: 'text-btn', dataset: { test: `identity-unlink-${identity.provider}` },
                        onclick: () => unlink(identity.provider, label(identity.provider)),
                    }, 'Unlink')))
                : h('li', { class: 'muted' }, 'No linked accounts.')),
            providers.filter(provider => !held.has(provider.id)).map(provider => h('button', {
                type: 'button', class: 'secondary', dataset: { test: `identity-link-${provider.id}` },
                onclick: () => link(provider.id, keep.querySelector('input').checked),
            }, `Link ${provider.label}`)),
            providers.length ? keep : h('p', { class: 'muted small' }, 'This server has no sign-in providers configured.'));
    }

    // Linking leaves the console (Task 9): the server answers where to send
    // the browser (`{url}`, the provider's sign-in page, with the flow
    // cookie set), and its callback comes back to /ui/#/account.
    async function link(provider, keep) {
        let answer;
        try {
            answer = await api.post(`/me/identities/${enc(provider)}/link`, { keep_sessions: keep });
        } catch { return; /* toasted */ }
        location.assign(answer.url);
    }

    async function unlink(provider, name) {
        const ok = await confirmDialog({
            title: `Unlink ${name}?`,
            message: `You can no longer sign in through ${name} until you link it again.`,
            action: 'Unlink',
        });
        if (!ok) return;
        try { await api.del(`/me/identities/${enc(provider)}`); } catch { return; /* toasted: 409 for the last method */ }
        toast(`Unlinked ${name}.`, 'ok');
        if (main.isConnected) await drawIdentities();
    }

    async function drawSessions() {
        let list;
        try { ({ sessions: list } = await api.get('/me/sessions')); } catch { return; }
        if (!main.isConnected) return;
        // One Sign out per session (Task 10b has no "all others" route).
        // This browser's own signs the console out once the server ended it.
        fill(sessions.body,
            sessionTable({
                id: 'session-table', prefix: 'session', sessions: list, revokeCurrent: true,
                revoke: entry => (entry.current ? signOutHere(entry) : signOutOther(entry)),
            }));
    }

    async function signOutOther(entry) {
        try { await api.del(`/me/sessions/${enc(entry.id_prefix)}`); } catch { return; /* toasted */ }
        toast('Signed that session out.', 'ok');
        if (main.isConnected) await drawSessions();
    }

    async function signOutHere(entry) {
        const ok = await confirmDialog({
            title: 'Sign this browser out?',
            message: 'This ends the session you are using now. Sign in again to come back.',
            action: 'Sign out',
        });
        if (!ok) return;
        // A 401 means it had ended already: the 401 handler shows the card.
        try { await api.del(`/me/sessions/${enc(entry.id_prefix)}`); } catch { return; /* toasted */ }
        signedOut();
    }

    async function drawSignIns() {
        let list;
        try { ({ sign_ins: list } = await api.get('/me/sign-ins')); } catch { return; }
        if (main.isConnected) fill(signIns.body, signInTable({ id: 'sign-in-table', signIns: list }));
    }

    await Promise.all([drawAccount(), drawPasskeys(), drawIdentities(), drawSessions(), drawSignIns()]);
}

// A refusal's reason as the server gave it, without the "forbidden: "
// prefix its 403s carry.
function reason(error) {
    return String(error?.body?.message ?? error?.message ?? error).replace(/^forbidden: /, '');
}

// A titled section of the page and the body its draw function fills.
function section(id, title, intro = null) {
    const body = h('div', { class: 'account-body' });
    const el = h('section', { id, class: 'account-section' },
        h('h2', {}, title), intro ? h('p', { class: 'muted small' }, intro) : null, body);
    return { el, body };
}

// The "keep my other sessions" box: unticked, a new sign-in method signs
// every other session out.
function keepSessionsBox(id) {
    return h('label', { class: 'small' },
        h('input', { type: 'checkbox', id, name: 'keep_sessions' }), ' Keep me signed in elsewhere');
}
