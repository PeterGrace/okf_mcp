// Search: ranked concepts with snippets, across every catalog the key
// reaches. Route: #/search/<query>, so a search can be linked and reloaded.
import { h, clear } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { go, browseHash } from '../core/router.js';
import { count } from '../core/format.js';

export async function render(main, rest) {
    const query = rest.join('/');
    const catalogs = store.whoami?.catalogs ?? [];
    const input = h('input', {
        type: 'search', id: 'search-query', name: 'q', value: query, autocomplete: 'off',
        placeholder: 'Words that must all occur', 'aria-label': 'Search',
    });
    const form = h('form', {
        id: 'search-form',
        role: 'search',
        onsubmit: event => {
            event.preventDefault();
            const text = input.value.trim();
            go(text ? `#/search/${enc(text)}` : '#/search');
        },
    }, h('fieldset', { role: 'group' }, input, h('button', { type: 'submit' }, 'Search')));
    const results = h('div', { id: 'search-results' });
    main.append(
        h('header', { class: 'page-head' },
            h('div', {},
                h('h1', {}, 'Search'),
                h('p', { class: 'muted small' }, catalogs.length ? `Across ${catalogs.join(', ')}` : 'This key reaches no catalog.'))),
        form,
        results);
    input.focus();
    if (!query) return;
    results.append(h('p', { class: 'muted' }, 'Searching...'));
    let found;
    try {
        found = await api.get(`/search?q=${enc(query)}&catalogs=${catalogs.map(enc).join(',')}&limit=50`);
    } catch {
        clear(results);
        return;
    }
    clear(results).append(
        h('p', { class: 'muted small', id: 'search-count' }, count(found.hits.length, 'hit')),
        h('ol', { class: 'hits' }, found.hits.map(hit => h('li', { class: 'hit' },
            h('a', { class: 'hit-title', href: browseHash(hit.catalog, hit.path), dataset: { catalog: hit.catalog, path: hit.path } },
                hit.title ?? hit.path),
            h('span', { class: 'mono small muted' }, ` ${hit.catalog} ${hit.path}`),
            hit.description ? h('p', { class: 'small' }, hit.description) : null,
            hit.snippet ? h('p', { class: 'snippet small muted' }, hit.snippet) : null))));
}
