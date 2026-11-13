// Access levels as the console shows them: the pill kinds, and a key's or
// grant's per-catalog levels.
import { h, pill } from '../core/dom.js';

export const ACCESS_KIND = { read: 'muted', write: 'info', admin: 'warn' };
const ORDER = ['read', 'write', 'admin'];

export function accessPill(access) {
    return pill(access, ACCESS_KIND[access]);
}

// A key's or grant's catalogs, from its `catalog_access` ({catalog: level},
// Task 2's KeyView). A self-issued one is capped at the account's
// `ceiling` ({catalog: level}, as GET /api/v1/session gives it): where the
// ceiling lowers a level, or no longer reaches the catalog, the cell says
// so (the server caps the credential at its next key reload). An
// admin-issued one keeps its levels.
export function levelsCell(credential, ceiling = {}) {
    const granted = Object.entries(credential.catalog_access ?? {});
    if (!granted.length) return '-';
    return h('ul', { class: 'level-list' }, granted.map(([catalog, access]) => {
        const allowed = credential.self_issued ? ceiling[catalog] : access;
        let note = null;
        if (!allowed) note = h('span', { class: 'muted small' }, ' (outside your ceiling now)');
        else if (ORDER.indexOf(allowed) < ORDER.indexOf(access)) note = h('span', { class: 'muted small' }, ` (capped to ${allowed})`);
        return h('li', { dataset: { catalog } }, h('span', { class: 'mono' }, catalog), ' ', accessPill(access), note);
    }));
}
