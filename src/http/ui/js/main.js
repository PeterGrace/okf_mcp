// Boot: theme, sign-in, the shell, the router.
import { $ } from './core/dom.js';
import { store } from './core/store.js';
import { api, ApiError, authHeaders, setUnauthorizedHandler } from './core/api.js';
import { start, route } from './core/router.js';
import { closeDrawer } from './core/drawer.js';
import { toast } from './core/toast.js';
import { refreshIdentity, refreshUnassigned, setSignedOutHandler } from './core/shell.js';
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
    // Signing in with an account comes back to this console page; /signin
    // keeps only a same-origin path such as this one (`safe_return_to`).
    $('#login-signin').href = `/signin?return_to=${encodeURIComponent(`/ui/${location.hash}`)}`;
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
    store.session = null;
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
    // The key wins over any cookie, in this browser as on the server.
    store.session = null;
    store.csrf = null;
    event.target.reset();
    await enterOrRecover();
});
// Ends this browser's console session, if it has one, whatever else is
// signed in: a stored key and a cookie session can coexist (a key pasted
// earlier, then a sign-in at /signin in another tab), and the cookie is
// HttpOnly, so only the server can say. Both requests go without the key
// (`authHeaders` with `key: null`), so they speak for the cookie alone.
// Resolves once no session is left: 2xx, or a 401 (none, or already over).
// Throws on anything else, since the session may then live on.
async function endCookieSession() {
    const found = await fetch('/api/v1/session', { headers: authHeaders('GET', { key: null }) });
    if (found.status === 401) return;
    if (!found.ok) throw new Error(`HTTP ${found.status}`);
    const { csrf } = await found.json();
    const ended = await fetch('/api/v1/session', {
        method: 'DELETE',
        headers: authHeaders('DELETE', { key: null, csrf }),
    });
    if (!ended.ok && ended.status !== 401) throw new Error(`HTTP ${ended.status}`);
}

// Forgets the credential once the server has no session left for it, and
// shows the card at the console's root. The Sign out button calls it, and
// so does the Account page after signing this browser's session out.
function signedOut() {
    store.key = null;
    store.whoami = null;
    store.csrf = null;
    store.session = null;
    history.replaceState(null, '', '/ui/');
    showLogin();
}
setSignedOutHandler(signedOut);

$('#signout').addEventListener('click', async () => {
    // The session ends on the server first. If that fails (the network, a
    // 5xx) nothing is cleared: the console stays as it is and says to try
    // again, because a reload would otherwise sign straight back in.
    try {
        await endCookieSession();
    } catch (error) {
        toast(`Sign-out failed: ${error.message}. Try again.`, 'error');
        return;
    }
    signedOut();
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

// Boot. One credential is in use at a time, and a live console session
// wins over a stored key: a session exists only after an account sign-in,
// which is newer than any key pasted before it (the card is not shown while
// a session resumes). The key is then dropped, so it can never come back
// once the session ends. Without a session, a stored key is used; without
// either, the card. A plain fetch, so a 401 here is not "the key was
// refused".
async function boot() {
    let response = null;
    try {
        response = await fetch('/api/v1/session', { headers: authHeaders('GET', { key: null }) });
    } catch { /* the server is down */ }
    if (response?.ok) {
        store.session = await response.json();
        store.csrf = store.session.csrf;
        store.key = null;
        await enterOrRecover();
    } else if (store.key) {
        await enterOrRecover();
    } else {
        showLogin(response ? undefined : 'The server could not be reached. Reload to retry.');
    }
}

applyTheme(store.theme);
boot();
