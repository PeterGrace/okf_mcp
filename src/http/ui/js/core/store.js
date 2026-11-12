// Browser-side state: the signed-in key and preferences, the whoami answer
// and a console session's CSRF token. Values live in memory; localStorage
// only persists the key and preferences across reloads (never the token,
// which the console reads again from /api/v1/session at boot).
// Storage can be unavailable or throw (private windows, blocked site data),
// and then the console still works for this page's lifetime.

const KEY = 'okf.key';
const CATALOG = 'okf.catalog';
const THEME = 'okf.theme';
const LEVELS = ['read', 'write', 'admin'];
const memory = new Map();

function read(name) {
    if (!memory.has(name)) {
        let stored = null;
        try { stored = localStorage.getItem(name); } catch { /* unavailable: memory only */ }
        memory.set(name, stored);
    }
    return memory.get(name);
}
function write(name, value) {
    memory.set(name, value ?? null);
    try {
        if (value === null || value === undefined) localStorage.removeItem(name);
        else localStorage.setItem(name, value);
    } catch { /* unavailable: the value lives in memory only */ }
}

export const store = {
    whoami: null,
    csrf: null,
    get key() { return read(KEY); },
    set key(value) { write(KEY, value); },
    get catalog() { return read(CATALOG); },
    set catalog(value) { write(CATALOG, value); },
    get theme() { return read(THEME); },
    set theme(value) { write(THEME, value); },
    can(level) {
        return !!this.whoami && LEVELS.indexOf(this.whoami.access) >= LEVELS.indexOf(level);
    },
};
