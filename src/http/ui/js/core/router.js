// Hash routing: '#/<page>/<rest...>' loads js/pages/<page>.js and calls its
// render(view, rest). /ui/manifest.json says which pages this build has, so
// a nav link to a page not built yet is hidden, never a 404.
//
// Each navigation renders into its own fresh container (`view`), and the
// previous one is detached from #main. A render still awaiting an API answer
// when the user moves on therefore writes into a detached element, never
// into the newer page. Pages check `view.isConnected` after an await before
// touching anything outside `view` (the drawer, the nav badge).
import { h } from './dom.js';

let pages = new Set();
let started = false;
const DEFAULTS = ['health', 'browse', 'keys'];

export function parse(hash) {
    const parts = hash.replace(/^#\/?/, '').split('/').filter(Boolean).map(decodeURIComponent);
    return { page: parts.shift() ?? '', rest: parts };
}

// '#/browse/<catalog>/<path segments>[/<section slug>]', each segment encoded
// (parse() decodes each one). src/render.rs (Task 4) writes the same shape
// into rendered links. path may start with '/' or not.
export function browseHash(catalog, path = '', section = null) {
    const parts = [catalog, ...String(path).replace(/^\/+/, '').split('/').filter(Boolean)];
    if (section) parts.push(section);
    return '#/browse/' + parts.map(encodeURIComponent).join('/');
}

export function go(hash) {
    if (location.hash === hash) route();
    else location.hash = hash;
}

// Replaces #main's content with a fresh view and runs render(view, rest) in it.
// Exported for the router journey in tests/ui_e2e/login.rs.
export async function mount(render, rest) {
    const view = h('div', { class: 'view' });
    document.getElementById('main').replaceChildren(view);
    await render(view, rest);
}

export async function route() {
    let { page, rest } = parse(location.hash);
    if (!page) page = DEFAULTS.find(name => pages.has(name)) ?? '';
    document.querySelectorAll('.nav-link').forEach(link => link.classList.toggle('active', link.dataset.nav === page));
    if (!pages.has(page)) {
        await mount(view => view.append(h('h1', {}, page ? 'Not available yet' : 'Welcome'),
            h('p', { class: 'muted' }, page ? `This build has no ${page} page.` : 'No pages are installed in this build yet.')), rest);
        return;
    }
    const placeholder = location.hash;
    const module = await import(`../pages/${page}.js`);
    if (location.hash !== placeholder) return;   // the user moved on while it loaded
    await mount(module.render, rest);
}

export async function start() {
    const manifest = await (await fetch('/ui/manifest.json')).json();
    pages = new Set(manifest.pages);
    document.querySelectorAll('.nav-link').forEach(link => { if (!pages.has(link.dataset.nav)) link.hidden = true; });
    if (!started) {
        addEventListener('hashchange', route);
        started = true;
    }
    await route();
}
