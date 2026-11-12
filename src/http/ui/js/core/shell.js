// The signed-in identity in the shell: who, which catalogs, which groups,
// and the unassigned count on the Users link.
// Pages that change keys or catalogs call refreshIdentity() afterwards, and
// those that assign a user or owner call refreshUnassigned().
import { $, h } from './dom.js';
import { store } from './store.js';
import { api } from './api.js';

export async function refreshIdentity() {
    store.whoami = await api.get('/whoami');
    const me = store.whoami;
    $('#whoami').textContent = `${me.user ? `${me.user} / ` : ''}${me.name ?? me.key_id} - ${me.access}`;
    document.querySelectorAll('[data-need]').forEach(el => { el.hidden = !store.can(el.dataset.need); });
    if (!me.catalogs.includes(store.catalog)) {
        store.catalog = me.catalogs.includes(me.default_catalog) ? me.default_catalog : (me.catalogs[0] ?? null);
    }
    const select = $('#catalog-select');
    select.replaceChildren(...me.catalogs.map(name => h('option', { value: name }, name)));
    select.value = store.catalog ?? '';
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
