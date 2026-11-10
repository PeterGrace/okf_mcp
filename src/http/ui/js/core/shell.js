// The signed-in identity in the shell: who, which catalogs, which groups.
// Pages that change keys or catalogs call refreshIdentity() afterwards.
import { $, h } from './dom.js';
import { store } from './store.js';
import { api } from './api.js';

export async function refreshIdentity() {
    store.whoami = await api.get('/whoami');
    const me = store.whoami;
    $('#whoami').textContent = `${me.name ?? me.key_id} - ${me.access}`;
    document.querySelectorAll('[data-need]').forEach(el => { el.hidden = !store.can(el.dataset.need); });
    if (!me.catalogs.includes(store.catalog)) {
        store.catalog = me.catalogs.includes(me.default_catalog) ? me.default_catalog : (me.catalogs[0] ?? null);
    }
    const select = $('#catalog-select');
    select.replaceChildren(...me.catalogs.map(name => h('option', { value: name }, name)));
    select.value = store.catalog ?? '';
}
