// Boot: theme, sign-in, the shell, the router.
import { $ } from './core/dom.js';
import { store } from './core/store.js';
import { api, ApiError, setUnauthorizedHandler } from './core/api.js';
import { start, route } from './core/router.js';
import { closeDrawer } from './core/drawer.js';
import { refreshIdentity } from './core/shell.js';

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
    store.key = null;
    store.whoami = null;
    showLogin('The key was refused. Sign in again.');
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
$('#signout').addEventListener('click', () => {
    store.key = null;
    store.whoami = null;
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
});
$('#drawer-scrim').addEventListener('click', closeDrawer);
addEventListener('keydown', event => { if (event.key === 'Escape') closeDrawer(); });

applyTheme(store.theme);
if (store.key) enterOrRecover();
else showLogin();
