// DOM helpers. Data reaches the page only through h(), fill() and
// textContent, never innerHTML, so catalog text can never become markup.

export const $ = (selector, root = document) => root.querySelector(selector);

// Children as the DOM takes them: nested arrays flattened, null, undefined
// and false dropped, anything not a Node turned into text. The DOM's own
// append and replaceChildren print null as the text "null" (issue #302),
// so every conditional child goes through h() or fill(), never straight
// to the native methods.
function nodes(children) {
    return children
        .flat(Infinity)
        .filter(child => child !== null && child !== undefined && child !== false)
        .map(child => (child instanceof Node ? child : String(child)));
}

// h('a', { href: '#/x', class: 'nav', onclick: fn }, 'text', child, [more])
export function h(tag, attrs = {}, ...children) {
    const el = document.createElement(tag);
    for (const [name, value] of Object.entries(attrs ?? {})) {
        if (value === false || value === null || value === undefined) continue;
        if (name.startsWith('on') && typeof value === 'function') el.addEventListener(name.slice(2), value);
        else if (name === 'dataset') Object.assign(el.dataset, value);
        else el.setAttribute(name, value === true ? '' : String(value));
    }
    el.append(...nodes(children));
    return el;
}

// Replaces el's children, filtered as h() filters them. Returns el.
export function fill(el, ...children) {
    el.replaceChildren(...nodes(children));
    return el;
}

export function clear(el) {
    el.replaceChildren();
    return el;
}

// kind: ok | warn | error | info | muted
export function pill(text, kind = 'muted') {
    return h('span', { class: `pill pill-${kind}` }, text);
}

// One term and its value, for a <dl class="facts">.
export function fact(label, value) {
    return [h('dt', {}, label), h('dd', {}, value)];
}
