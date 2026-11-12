import { h, clear } from './dom.js';

// Bumped by every drawerTicket(), openDrawer() and closeDrawer(): a drawer
// load that awaited the server is stale once anything else has claimed,
// filled or dismissed the drawer since it started.
let generation = 0;

// A claim on the drawer, taken when a drawer is asked for (before any await)
// and kept by everything that drawer does. Taking one invalidates every
// older claim. `ticket.current()` is true only while no other claim, open or
// close has happened since, and `view` (the page's container) is still
// shown. `ticket.open(...)` opens the drawer as openDrawer does and keeps
// the claim, so the drawer's own actions can re-render or close it later:
// they do so only while `ticket.current()`. A dismissed or replaced drawer
// is never reopened by a late answer.
export function drawerTicket(view) {
    let mine = ++generation;
    return {
        current: () => mine === generation && view.isConnected,
        open(title, subtitle, ...content) {
            const body = openDrawer(title, subtitle, ...content);
            mine = generation;
            return body;
        },
        close() {
            if (mine === generation) closeDrawer();
        },
    };
}

// Opens the right-hand drawer and returns its body element for the caller to fill.
export function openDrawer(title, subtitle, ...content) {
    generation++;
    const drawer = document.getElementById('drawer');
    const body = h('div', { class: 'drawer-body' }, ...content);
    clear(drawer).append(
        h('header', { class: 'drawer-head' },
            h('div', {},
                h('h2', { class: 'mono', id: 'drawer-title' }, title),
                subtitle ? h('p', { class: 'muted small' }, subtitle) : null),
            h('button', { class: 'text-btn', type: 'button', id: 'drawer-close', onclick: closeDrawer }, 'Close')),
        body);
    drawer.hidden = false;
    document.getElementById('drawer-scrim').hidden = false;
    return body;
}

export function closeDrawer() {
    generation++;
    document.getElementById('drawer').hidden = true;
    document.getElementById('drawer-scrim').hidden = true;
}
