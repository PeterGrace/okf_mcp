import { h } from '../core/dom.js';

// A modal confirmation; resolves true or false. With `typed`, the action
// stays disabled until the user types that exact text (used for deletes).
export function confirmDialog({ title, message, action = 'Confirm', danger = true, typed = null }) {
    return new Promise(resolve => {
        const input = typed
            ? h('input', { type: 'text', autocomplete: 'off', id: 'confirm-typed', 'aria-label': `Type ${typed} to confirm` })
            : null;
        const ok = h('button', { type: 'button', id: 'confirm-ok', class: danger ? 'danger' : null, disabled: typed ? true : null }, action);
        const cancel = h('button', { type: 'button', id: 'confirm-cancel', class: 'secondary' }, 'Cancel');
        const dialog = h('dialog', { class: 'confirm' },
            h('article', {},
                h('h3', {}, title),
                h('p', {}, message),
                typed ? h('p', { class: 'small muted' }, 'Type ', h('code', {}, typed), ' to confirm.') : null,
                input,
                h('footer', {}, cancel, ok)));
        const done = value => { dialog.close(); dialog.remove(); resolve(value); };
        if (input) input.addEventListener('input', () => { ok.disabled = input.value !== typed; });
        ok.addEventListener('click', () => done(true));
        cancel.addEventListener('click', () => done(false));
        dialog.addEventListener('cancel', event => { event.preventDefault(); done(false); });
        document.body.append(dialog);
        dialog.showModal();
    });
}
