import { h } from '../core/dom.js';

// A segmented control. options: [{ value, label }]; onChange(value) on a click.
export function segments(options, value, onChange) {
    const wrap = h('div', { class: 'segments', role: 'tablist' });
    const draw = () => wrap.replaceChildren(...options.map(option => h('button', {
        type: 'button',
        role: 'tab',
        class: option.value === value ? 'seg active' : 'seg',
        'aria-selected': String(option.value === value),
        dataset: { value: option.value },
        onclick: () => { value = option.value; draw(); onChange(value); },
    }, option.label)));
    draw();
    return wrap;
}
