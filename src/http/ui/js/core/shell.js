// The signed-in identity in the shell: who, which catalogs, which groups,
// and the unassigned count on the Users link; and the operator's brand.
// Pages that change keys or catalogs call refreshIdentity() afterwards, and
// those that assign a user or owner call refreshUnassigned().
import { $, h } from './dom.js';
import { store } from './store.js';
import { api } from './api.js';

export async function refreshIdentity() {
    // Signed in with an account, the session (admin flag, ceiling, CSRF
    // token) is read again beside whoami: an admin may have changed it.
    const [session, me] = await Promise.all([
        store.session ? api.get('/session') : null,
        api.get('/whoami'),
    ]);
    if (session) {
        store.session = session;
        store.csrf = session.csrf;
    }
    store.whoami = me;
    // A console session is its user; a key shows its user and its name.
    $('#whoami').textContent = me.kind === 'session'
        ? `${me.user} - ${me.access}`
        : `${me.user ? `${me.user} / ` : ''}${me.name ?? me.key_id} - ${me.access}`;
    if (!me.catalogs.includes(store.catalog)) {
        store.catalog = me.catalogs.includes(me.default_catalog) ? me.default_catalog : (me.catalogs[0] ?? null);
    }
    // After the catalog is settled: a member's level depends on it.
    document.querySelectorAll('[data-need]').forEach(el => { el.hidden = !shows(el.dataset.need); });
    const select = $('#catalog-select');
    select.replaceChildren(...me.catalogs.map(name => h('option', { value: name }, name)));
    select.value = store.catalog ?? '';
}

// Whether an element marked data-need="<need>" shows: "session" for the
// account pages (signed in with an account, not a key), else an access
// level.
function shows(need) {
    return need === 'session' ? !!store.session : store.can(need);
}

// The account pages (My keys, My grants, Account) are for a person signed
// in with their account. Signed in by key, the page says so in a muted
// paragraph with this `id`. Returns whether the page may render.
export function requireSession(main, id) {
    if (store.session) return true;
    main.append(h('p', { class: 'muted', id },
        'This page is for people signed in with their account. Sign out, then sign in with your account to use it.'));
    return false;
}

// GET /api/v1/me, fresh on every load of an account page: the ceiling and
// everything else it shows come from this answer, never from the sign-in
// snapshot, since an admin may have changed them since. Its ceiling
// ([{catalog, access}]) replaces the session's ({catalog: level}, as GET
// /api/v1/session spells it), so the cap notes and `store.can` follow too.
// Rejects as api.get does (already toasted).
export async function freshAccount() {
    const account = await api.get('/me');
    if (store.session) {
        store.session.ceiling = Object.fromEntries(account.ceiling.map(({ catalog, access }) => [catalog, access]));
    }
    return account;
}

// How the console leaves a session it ended itself: main.js installs it
// (the shell's Sign out uses the same one), and the Account page calls
// signedOut() after signing this browser's own session out.
let leave = () => {};
export function setSignedOutHandler(handler) { leave = handler; }
export function signedOut() { leave(); }

// After a key's user changed: when it is the signed-in key, its user is
// shell state (#whoami, the Users page's "you" marker), so reload whoami.
// Every path that assigns a key calls this.
export async function keyUserChanged(keyId) {
    if (keyId === store.whoami?.key_id) await refreshIdentity().catch(() => {});
}

// Whether this key may administer users, teams and ownership: an admin key
// that reaches every catalog (plan Decision 3). Others see those controls
// disabled.
export function canAdministerPeople() {
    return store.can('admin') && !!store.whoami?.all_catalogs;
}

let unassignedTicket = 0;

// The count of keys, grants, codes and catalogs with no user or owner, on
// the Users link, for keys that may assign them; hidden at zero and for
// everyone else. Only the newest call writes, as for the findings badge.
export async function refreshUnassigned() {
    const badge = document.getElementById('unassigned-badge');
    if (!badge) return;
    const mine = ++unassignedTicket;
    if (!canAdministerPeople()) { badge.hidden = true; return; }
    let rows = null;
    try { rows = await api.get('/unassigned', { quiet: true }); } catch { /* hidden below */ }
    if (mine !== unassignedTicket) return;
    const total = rows ? rows.keys.length + rows.codes.length + rows.catalogs.length : 0;
    badge.textContent = String(total);
    badge.hidden = total === 0;
}

let brandTicket = 0;

// The operator's product name (Settings: product_name) on the login card, in
// the sidebar and in the tab title, from the public /ui/brand.json. Called at
// boot (before anyone signs in) and after the setting is saved or reset.
// textContent only. Only the newest call writes: a boot read answering late
// never draws over a newer save's. On any failure the page keeps what it
// shows (index.html's "okf" until the first answer).
export async function applyBrand() {
    const mine = ++brandTicket;
    let brand = null;
    try {
        const response = await fetch('/ui/brand.json', { cache: 'no-store' });
        if (response.ok) brand = await response.json();
    } catch { /* keep the current name */ }
    if (mine !== brandTicket || !brand?.product_name) return;
    document.title = `${brand.product_name} console`;
    for (const el of document.querySelectorAll('.brand')) el.textContent = brand.product_name;
}
