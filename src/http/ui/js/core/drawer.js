import { h, clear } from './dom.js';

// Bumped by every drawerTicket(), openDrawer() and closeDrawer(): a drawer
// load that awaited the server is stale once anything else has claimed,
// filled or dismissed the drawer since it started.
let generation = 0;

// A claim on the drawer for a load that awaits before openDrawer(). Taking
// one invalidates every older pending load. `ticket.current()` is true only
// while no other load, open or close has happened since, and `view` (the
// page's container) is still shown. Check it after every await, before
// filling the drawer.
export function drawerTicket(view) {
    const mine = ++generation;
    return { current: () => mine === generation && view.isConnected };
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
