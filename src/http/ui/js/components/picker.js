import { h } from '../core/dom.js';
import { api } from '../core/api.js';

// The names of every user or team, for a picker: `kind` is 'users' or
// 'teams', the collection GET /<kind> answers with. Any admin may list them
// (plan Decision 3). Throws the toasted ApiError on failure.
export async function namesOf(kind) {
    return (await api.get(`/${kind}`))[kind].map(row => row.name);
}

// A <select> over `names`, with `selected` chosen. Without one, a disabled
// empty first option is chosen, so a `required` picker refuses to submit
// until someone picks a name.
export function picker(attrs, names, selected = null) {
    return h('select', attrs,
        h('option', { value: '', disabled: true, selected: !selected }, 'Choose...'),
        names.map(name => h('option', { value: name, selected: name === selected }, name)));
}

// A picker plus a button that calls `assign(name)`: the Assign or Change user
// (or owner) control of a drawer or the unassigned view. The button is
// enabled only while the picker names someone other than `current`, and
// only when `allowed`. `ids` gives the select's and the button's ids, if any.
export function assignControl({ names, current = null, label, allowed = true, ids = {}, assign }) {
    const select = picker({ id: ids.select ?? null, 'aria-label': label, disabled: !allowed }, names, current);
    const button = h('button', { type: 'button', id: ids.button ?? null, disabled: true }, label);
    const sync = () => { button.disabled = !allowed || !select.value || select.value === current; };
    select.addEventListener('change', sync);
    button.addEventListener('click', async () => {
        button.disabled = true;
        try { await assign(select.value); } finally { sync(); }
    });
    return h('div', { class: 'inline-form' }, select, button);
}
