// The passkey buttons of the sign-in and setup pages (src/http/signin.rs).
// Each page has at most one; its data-return-to says where to go after.
import { assertionJson, creationOptions, registrationJson, requestOptions } from './webauthn.js';

async function post(path, body) {
    const response = await fetch(path, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
    });
    let json = null;
    try { json = await response.json(); } catch { /* not JSON */ }
    if (!response.ok) throw new Error(json?.message ?? `HTTP ${response.status}`);
    return json;
}

// Runs one ceremony from `button`, showing a failure under it.
function wire(button, ceremony) {
    const error = document.getElementById('passkey-error');
    if (!window.PublicKeyCredential) {
        button.disabled = true;
        error.textContent = 'This browser does not support passkeys.';
        error.hidden = false;
        return;
    }
    button.addEventListener('click', async () => {
        button.disabled = true;
        error.hidden = true;
        try {
            const done = await ceremony(button.dataset.returnTo);
            location.assign(done.redirect);
        } catch (failure) {
            // A dismissed prompt is a NotAllowedError; say so plainly.
            error.textContent = failure.name === 'NotAllowedError'
                ? 'The passkey prompt was closed. Try again.'
                : failure.message;
            error.hidden = false;
            button.disabled = false;
        }
    });
    button.dataset.ready = 'true';
}

const signIn = document.getElementById('signin-passkey');
if (signIn) {
    wire(signIn, async returnTo => {
        const challenge = await post('/auth/passkey/signin/start', { return_to: returnTo });
        const credential = await navigator.credentials.get(requestOptions(challenge));
        return post('/auth/passkey/signin/finish', assertionJson(credential));
    });
}

const setup = document.getElementById('setup-passkey');
if (setup) {
    wire(setup, async returnTo => {
        const token = new URLSearchParams(location.search).get('token') ?? '';
        const name = document.getElementById('setup-passkey-name').value;
        const challenge = await post('/auth/passkey/setup/start', { token, name, return_to: returnTo });
        const credential = await navigator.credentials.create(creationOptions(challenge));
        return post('/auth/passkey/setup/finish', registrationJson(credential));
    });
}
