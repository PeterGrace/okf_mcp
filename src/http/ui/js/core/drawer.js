import { h, clear } from './dom.js';

// Opens the right-hand drawer and returns its body element for the caller to fill.
export function openDrawer(title, subtitle, ...content) {
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
    document.getElementById('drawer').hidden = true;
    document.getElementById('drawer-scrim').hidden = true;
}
