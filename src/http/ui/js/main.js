// Boot: theme, sign-in, the shell, the router.
import { $ } from './core/dom.js';
import { store } from './core/store.js';
import { api, ApiError, setUnauthorizedHandler } from './core/api.js';
import { start, route } from './core/router.js';
import { closeDrawer } from './core/drawer.js';
import { refreshIdentity, refreshUnassigned } from './core/shell.js';
import { refreshFindingsBadge } from './pages/findings.js';

// Surface script errors and CSP violations as console errors: the browser
// tests fail on any. An ApiError was already shown as a toast.
addEventListener('error', event => console.error('script error:', event.message));
addEventListener('unhandledrejection', event => {
    if (event.reason instanceof ApiError) { event.preventDefault(); return; }
    console.error('unhandled rejection:', event.reason?.message ?? String(event.reason));
});
document.addEventListener('securitypolicyviolation', event =>
    console.error('CSP violation:', event.violatedDirective, event.blockedURI));

function applyTheme(theme) {
    const chosen = theme ?? (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light');
    document.documentElement.dataset.theme = chosen;
}

function showLogin(message) {
    $('#app').hidden = true;
    $('#login').hidden = false;
    const error = $('#login-error');
    error.textContent = message ?? '';
    error.hidden = !message;
}

async function enter() {
    await refreshIdentity();
    $('#login').hidden = true;
    $('#app').hidden = false;
    await start();
    refreshFindingsBadge();
    refreshUnassigned();
}

// enter(), but a failure never leaves a blank page: a refused key is already
// back on the login card (the 401 handler), and any other failure (the
// server down, a 500) shows the card with the reason. The stored key is
// kept, so a reload retries with it.
async function enterOrRecover() {
    try {
        await enter();
    } catch (error) {
        if (error instanceof ApiError && error.status === 401) return;
        showLogin(`The console could not load: ${error.message}. Reload to retry, or sign in again.`);
    }
}

setUnauthorizedHandler(() => {
    const hadKey = !!store.key;
    store.key = null;
    store.whoami = null;
    store.csrf = null;
    showLogin(hadKey ? 'The key was refused. Sign in again.' : 'Your session ended. Sign in again.');
});

$('#login-form').addEventListener('submit', async event => {
    event.preventDefault();
    const key = String(new FormData(event.target).get('key') ?? '').trim();
    try {
        await api.get('/whoami', { key, quiet: true });
    } catch (error) {
        showLogin(error.status === 401 ? 'That key was not accepted.' : `Sign-in failed: ${error.message}`);
        return;
    }
    store.key = key;
    event.target.reset();
    await enterOrRecover();
});
$('#signout').addEventListener('click', async () => {
    // A console session ends on the server; a pasted key only in this browser.
    if (!store.key && store.csrf) {
        try { await api.del('/session', { quiet: true }); } catch { /* already over */ }
    }
    store.key = null;
    store.whoami = null;
    store.csrf = null;
    history.replaceState(null, '', '/ui/');
    showLogin();
});
$('#theme-toggle').addEventListener('click', () => {
    const next = document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark';
    store.theme = next;
    applyTheme(next);
});
$('#catalog-select').addEventListener('change', event => {
    store.catalog = event.target.value;
    route();
    refreshFindingsBadge();
});
$('#drawer-scrim').addEventListener('click', closeDrawer);
addEventListener('keydown', event => { if (event.key === 'Escape') closeDrawer(); });

// Without a stored key, a console session (its cookie) may still be live:
// ask before showing the card. A plain fetch, so a 401 here is not "the key
// was refused".
async function resumeSession() {
    let response;
    try { response = await fetch('/api/v1/session'); } catch { showLogin(); return; }
    if (!response.ok) { showLogin(); return; }
    store.csrf = (await response.json()).csrf;
    await enterOrRecover();
}

applyTheme(store.theme);
if (store.key) enterOrRecover();
else resumeSession();
