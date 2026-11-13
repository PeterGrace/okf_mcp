// The one time a secret is on screen: a modal that only the explicit
// button closes, after which the secret is removed from the page. Element
// ids start with `prefix` (`key-secret`, `code-secret-done`,
// `my-key-secret-dialog`, ...). The secret lives only in this dialog:
// never logged, never stored.
import { h } from '../core/dom.js';
import { toast } from '../core/toast.js';

export function showOnce({ prefix, title, warning, secret }) {
    let shown = secret; // dropped on close, so not even the copy handler keeps it
    const code = h('code', { id: `${prefix}-secret` }, shown);
    const copy = h('button', { type: 'button', id: `${prefix}-secret-copy`, class: 'secondary' }, 'Copy');
    const done = h('button', { type: 'button', id: `${prefix}-secret-done` }, 'I have stored it');
    const dialog = h('dialog', { id: `${prefix}-secret-dialog`, class: 'confirm' },
        h('article', {},
            h('h3', {}, title),
            h('p', { class: 'warn-text' }, warning),
            h('pre', { class: 'secret' }, code),
            h('footer', {}, copy, done)));
    copy.addEventListener('click', async () => {
        try {
            // navigator.clipboard exists only in secure contexts (https or localhost).
            await navigator.clipboard.writeText(shown);
            toast('Copied to the clipboard.', 'ok');
        } catch {
            toast('Copying failed: select the text and copy it by hand.', 'error');
        }
    });
    done.addEventListener('click', () => {
        shown = null;
        code.textContent = '';
        dialog.close();
        dialog.remove();
    });
    dialog.addEventListener('cancel', event => event.preventDefault()); // Escape does not close it
    document.body.append(dialog);
    dialog.showModal();
}
