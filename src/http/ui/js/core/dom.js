// DOM helpers. Data reaches the page only through h() and textContent, never
// innerHTML, so catalog text can never become markup.

export const $ = (selector, root = document) => root.querySelector(selector);

// h('a', { href: '#/x', class: 'nav', onclick: fn }, 'text', child, [more])
export function h(tag, attrs = {}, ...children) {
    const el = document.createElement(tag);
    for (const [name, value] of Object.entries(attrs ?? {})) {
        if (value === false || value === null || value === undefined) continue;
        if (name.startsWith('on') && typeof value === 'function') el.addEventListener(name.slice(2), value);
        else if (name === 'dataset') Object.assign(el.dataset, value);
        else el.setAttribute(name, value === true ? '' : String(value));
    }
    for (const child of children.flat(Infinity)) {
        if (child === null || child === undefined || child === false) continue;
        el.append(child instanceof Node ? child : String(child));
    }
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
