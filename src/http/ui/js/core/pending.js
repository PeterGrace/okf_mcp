// Writes in flight, by a stable key naming what they change (for example
// `setting:max_keys_per_user`, `mail:<id>`, `user:alice:session:<prefix>`).
// The state lives here, not on a button, so a control drawn again while its
// write runs (a re-sorted table, a refreshed list, a reopened drawer) is
// drawn disabled too, and a second start of the same write is ignored.
//
// A page marks each control of a write with guard(el, key, allowed) and
// starts the write with once(key, write). While the write runs, every
// guarded control of that key in the document is disabled; when it
// settles, each is enabled again as its `allowed` says.

const pending = new Set();
// Each guarded control's `allowed`: a boolean, or a function read each
// time the control is updated. A WeakMap (keys held weakly) lets a
// control removed from the page be garbage-collected with its entry.
const allowedFor = new WeakMap();

// Whether the write `key` is running.
export function isPending(key) {
    return pending.has(key);
}

function update(el) {
    const allowed = allowedFor.get(el);
    el.disabled = !(typeof allowed === 'function' ? allowed() : allowed) || pending.has(el.dataset.pending);
}

// Marks `el` as a control of the write `key`: disabled while that write
// runs, and otherwise whenever `allowed` (a boolean, or a function) is
// false. Returns `el`.
export function guard(el, key, allowed = true) {
    el.dataset.pending = key;
    allowedFor.set(el, allowed);
    update(el);
    return el;
}

// Updates a guarded control again, after what its `allowed` function
// reads has changed.
export function recheck(el) {
    update(el);
}

function sync(key) {
    document.querySelectorAll('[data-pending]').forEach(el => {
        if (el.dataset.pending === key) update(el);
    });
}

// Runs `write` (an async function) unless the write `key` is already
// running, with every guarded control of `key` disabled until it settles.
// Resolves to write's result, or to undefined when it was already running.
export async function once(key, write) {
    if (pending.has(key)) return undefined;
    pending.add(key);
    sync(key);
    try {
        return await write();
    } finally {
        pending.delete(key);
        sync(key);
    }
}
