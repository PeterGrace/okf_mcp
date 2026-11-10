import { h } from './dom.js';

// kind: info | ok | error. Errors stay longer and are announced as alerts.
export function toast(message, kind = 'info') {
    const el = h('div', { class: `toast toast-${kind}`, role: kind === 'error' ? 'alert' : 'status' }, message);
    document.getElementById('toasts').append(el);
    setTimeout(() => el.remove(), kind === 'error' ? 8000 : 4000);
}
