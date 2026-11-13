// Settings (admin): the policy settings (each its effective value, and
// whether the database overrides the config file's default; Save and
// Reset), the configured OIDC providers and the mail provider (read-only:
// they live in the config file, and no secret is ever sent), Send test
// email, and the mail queue: mail not yet sent, with Retry and Delete
// (never a body or a link). Any admin reads it; changing anything needs an
// admin that reaches every catalog, as users and teams do.
//
// Every value shown comes from the server's latest answer: each action is
// followed by a fresh GET of what it changed. A refusal's reason (a value
// the setting refuses, 422 with `limit: "setting"`) stays beside what it
// refused until that control's next action succeeds.
import { h, pill, fact, fill } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../core/toast.js';
import { ago, when } from '../core/format.js';
import { canAdministerPeople } from '../core/shell.js';
import { listTable } from '../components/list-table.js';
import { confirmDialog } from '../components/confirm.js';

// What each setting does, under its key.
const MEANING = {
    password_sign_in: 'Offer email and password sign-in',
    max_keys_per_user: 'Live self-issued keys per user',
    max_key_lifetime: 'Longest self-issued key expiry ("none": no limit)',
    console_session_idle: 'Console session idle timeout',
    console_session_absolute: 'Console session absolute timeout',
    setup_link_ttl: 'Setup link lifetime',
    reset_link_ttl: 'Password reset link lifetime',
};
// GET /mail/queue's statuses, as pill kinds.
const MAIL_STATUS_KIND = { pending: 'info', sending: 'info', retrying: 'warn', failed: 'error' };
// The statuses a mail may be retried from (queue::retry takes Failed and
// Killed jobs), and the one it may not be deleted in (a worker holds it).
const RETRYABLE = new Set(['retrying', 'failed']);
const SENDING = 'sending';

export async function render(main) {
    const head = h('header', { class: 'page-head' },
        h('div', {},
            h('h1', {}, 'Settings'),
            h('p', { class: 'muted' }, 'Sign-in and self-service policy, the sign-in providers and mail, and the mail queue.')));
    if (!store.can('admin')) {
        fill(main, head, h('p', { class: 'muted', id: 'settings-denied' }, 'Settings need an admin.'));
        return;
    }
    const manager = canAdministerPeople();
    // The last refusal per control (a setting's key, 'mail-test', or
    // 'queue-<id>' for a queued mail), drawn until that control's next
    // success.
    const refusals = new Map();
    const refusalNote = (control, test) => (refusals.has(control)
        ? h('p', { class: 'error small', role: 'alert', dataset: { test } }, refusals.get(control))
        : null);

    // Runs `request`; on success toasts `message(answer)` and forgets the
    // control's refusal, on failure (already toasted) keeps its reason.
    // Then `reload()` reads the server's state again.
    const act = async (control, request, message, reload) => {
        try {
            toast(message(await request()), 'ok');
            refusals.delete(control);
        } catch (error) {
            refusals.set(control, error.message);
        }
        await reload();
    };

    const valueControl = row => {
        const attrs = { class: 'settings-input', 'aria-label': row.key, disabled: !manager,
                        dataset: { test: `setting-input-${row.key}` } };
        const input = row.key === 'password_sign_in'
            ? h('select', attrs, ['true', 'false'].map(value => h('option', { value, selected: value === row.value }, value)))
            : h('input', { ...attrs, value: row.value, autocomplete: 'off' });
        const save = h('button', {
            type: 'button', class: 'secondary', disabled: !manager, dataset: { test: `setting-save-${row.key}` },
            onclick: () => {
                save.disabled = true;
                const value = input.value.trim();
                act(row.key, () => api.put(`/settings/${enc(row.key)}`, { value }),
                    answer => `${answer.key} is now ${answer.value}.`, loadSettings);
            },
        }, 'Save');
        const reset = h('button', {
            type: 'button', class: 'text-btn', disabled: !manager || !row.overridden,
            dataset: { test: `setting-reset-${row.key}` },
            onclick: () => {
                reset.disabled = true;
                act(row.key, () => api.del(`/settings/${enc(row.key)}`),
                    answer => `${answer.key} is back to its default, ${answer.value}.`, loadSettings);
            },
        }, 'Reset');
        return [h('div', { class: 'inline-form' }, input, save, reset),
            refusalNote(row.key, `setting-error-${row.key}`)];
    };
    const settings = listTable([
        { key: 'key', label: 'Setting', sort: row => row.key,
          render: row => [h('code', { dataset: { test: `setting-${row.key}` } }, row.key),
              h('div', { class: 'muted small' }, MEANING[row.key] ?? '')] },
        { key: 'value', label: 'Value', render: valueControl },
        { key: 'overridden', label: 'Source',
          render: row => h('span', { dataset: { test: `setting-source-${row.key}` } },
              row.overridden ? pill('override', 'info') : pill('default', 'muted'),
              row.overridden && row.updated_by
                  ? h('span', { class: 'muted small' }, ` by ${row.updated_by}, ${ago(row.updated_at)}`)
                  : null) },
    ], [], { empty: 'No settings.' });
    settings.id = 'settings-table';

    const providers = h('div', { id: 'settings-oidc' });
    const mail = h('div', { id: 'settings-mail' });
    const to = h('input', { type: 'email', name: 'to', required: true, autocomplete: 'off',
                            placeholder: 'name@example.com', 'aria-label': 'Send a test email to', disabled: !manager });
    const send = h('button', { type: 'submit', disabled: !manager }, 'Send test email');
    const testForm = h('form', { id: 'mail-test-form', class: 'inline-form' }, to, send);
    const testRefusal = h('div', { id: 'mail-test-error' });
    testForm.addEventListener('submit', async event => {
        event.preventDefault();
        send.disabled = true;
        const address = to.value.trim();
        await act('mail-test', () => api.post('/mail/test', { to: address }),
            answer => `Queued a test email to ${answer.to}.`, loadQueue);
        if (!refusals.has('mail-test')) testForm.reset();
        fill(testRefusal, refusalNote('mail-test', 'mail-test-error'));
        send.disabled = !manager;
    });

    const queueAction = (row, request, message) => act(`queue-${row.id}`, request, message, loadQueue);
    const queue = listTable([
        { key: 'to', label: 'To', class: 'mono', sort: row => row.to },
        { key: 'template', label: 'Template', sort: row => row.template },
        { key: 'status', label: 'Status', sort: row => row.status,
          render: row => h('span', { dataset: { test: `mail-status-${row.id}` } }, pill(row.status, MAIL_STATUS_KIND[row.status])) },
        { key: 'attempts', label: 'Attempts', class: 'num', sort: row => row.attempts },
        { key: 'run_at', label: 'Next try', sort: row => row.run_at,
          render: row => (row.status === 'pending' || row.status === 'retrying' ? when(row.run_at) : '-') },
        { key: 'last_error', label: 'Last error',
          render: row => h('span', { class: 'small', dataset: { test: `mail-error-${row.id}` } }, row.last_error ?? '-') },
        { key: 'actions', label: '', render: row => [h('div', { class: 'inline-form' },
            RETRYABLE.has(row.status) ? h('button', {
                type: 'button', class: 'secondary', disabled: !manager, dataset: { test: `mail-retry-${row.id}` },
                onclick: () => queueAction(row, () => api.post(`/mail/queue/${enc(row.id)}/retry`),
                    () => `Sending the mail to ${row.to} again.`),
            }, 'Retry') : null,
            h('button', {
                type: 'button', class: 'text-btn', disabled: !manager || row.status === SENDING,
                title: row.status === SENDING ? 'It is being sent right now.' : null,
                dataset: { test: `mail-delete-${row.id}` },
                onclick: async () => {
                    const ok = await confirmDialog({
                        title: `Delete the mail to ${row.to}?`,
                        message: 'It is not sent. A setup or reset link in it stays valid until it expires.',
                        action: 'Delete',
                    });
                    if (ok) {
                        await queueAction(row, () => api.del(`/mail/queue/${enc(row.id)}`),
                            () => `Deleted the mail to ${row.to}.`);
                    }
                },
            }, 'Delete')),
            refusalNote(`queue-${row.id}`, `mail-refusal-${row.id}`)] },
    ], [], { empty: 'No mail waiting or failed.' });
    queue.id = 'mail-queue-table';

    fill(main,
        head,
        manager ? null : h('p', { class: 'muted small', id: 'settings-readonly' },
            'Changing settings or the mail queue needs an admin key that reaches every catalog.'),
        h('section', { class: 'account-section', id: 'settings-policy' }, h('h2', {}, 'Policy'),
            h('p', { class: 'muted small' }, 'The config file\'s [auth] table gives the defaults; a value saved here overrides it until Reset.'),
            settings),
        h('section', { class: 'account-section' }, h('h2', {}, 'Sign-in providers'), providers),
        h('section', { class: 'account-section' }, h('h2', {}, 'Mail'), mail, testForm, testRefusal),
        h('section', { class: 'account-section' },
            h('div', { class: 'page-head' },
                h('h2', {}, 'Mail queue'),
                h('div', { class: 'actions' },
                    h('button', { type: 'button', id: 'mail-queue-refresh', class: 'secondary', onclick: () => loadQueue() }, 'Refresh'))),
            h('p', { class: 'muted small' }, 'Mail waiting to be sent, and mail that failed after its retries (24 hours) or at once.'),
            queue));

    async function loadSettings() {
        let answer;
        try { answer = await api.get('/settings'); } catch { return; /* toasted */ }
        if (!main.isConnected) return;
        settings.redraw(answer.settings);
        fill(providers, answer.oidc.length
            ? h('ul', { class: 'grant-list' }, answer.oidc.map(provider =>
                h('li', { dataset: { test: `oidc-${provider.id}` } },
                    h('strong', {}, provider.label), ' ', h('code', {}, provider.id), ' ',
                    h('span', { class: 'mono small muted' }, provider.issuer))))
            : h('p', { class: 'muted small' }, 'No OIDC providers are configured: add [[auth.oidc]] tables to the config file.'));
        fill(mail, h('dl', { class: 'facts' },
            fact('Provider', h('code', {}, answer.mail.provider)),
            fact('From', answer.mail.from)));
    }

    async function loadQueue() {
        let answer;
        try { answer = await api.get('/mail/queue'); } catch { return; /* toasted */ }
        if (!main.isConnected) return;
        // A refusal for a mail no longer listed has nothing to stand beside.
        const listed = new Set(answer.mail.map(row => `queue-${row.id}`));
        for (const control of refusals.keys()) {
            if (control.startsWith('queue-') && !listed.has(control)) refusals.delete(control);
        }
        queue.redraw(answer.mail);
    }

    await Promise.all([loadSettings(), loadQueue()]);
}
