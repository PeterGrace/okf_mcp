// The one way pages talk to the server: /api/v1 with the stored bearer key,
// or else the console session's cookie (sent by the browser itself) with
// its CSRF token on writes. A non-2xx answer becomes an ApiError (already
// toasted unless `quiet`); a 401 also signs the browser out through the
// handler main.js installs.
import { store } from './store.js';
import { toast } from './toast.js';

export class ApiError extends Error {
    constructor(status, body) {
        super(body?.message ?? `HTTP ${status}`);
        this.status = status;
        this.body = body;
    }
}

let unauthorized = () => {};
export function setUnauthorizedHandler(handler) { unauthorized = handler; }

async function request(method, path, { body, raw = false, quiet = false, key } = {}) {
    const headers = {};
    const bearer = key ?? store.key;
    // A pasted key goes as a bearer. Without one, the session cookie goes
    // by itself (same origin), and a write carries the session's CSRF token.
    if (bearer) headers.Authorization = `Bearer ${bearer}`;
    else if (method !== 'GET' && store.csrf) headers['X-OKF-CSRF'] = store.csrf;
    let payload;
    if (body instanceof Blob) {
        payload = body;
        headers['Content-Type'] = 'application/gzip';
    } else if (body !== undefined) {
        payload = JSON.stringify(body);
        headers['Content-Type'] = 'application/json';
    }
    const response = await fetch(`/api/v1${path}`, { method, headers, body: payload });
    if (response.status === 401) {
        unauthorized();
        throw new ApiError(401, null);
    }
    if (!response.ok) {
        let errorBody = null;
        try { errorBody = await response.json(); } catch { /* not JSON */ }
        const error = new ApiError(response.status, errorBody);
        const denied = store.key ? 'Your key cannot do this.' : 'Your account cannot do this.';
        if (!quiet) toast(response.status === 403 ? denied : error.message, 'error');
        throw error;
    }
    if (raw) return response;
    return response.status === 204 ? null : response.json();
}

export const api = {
    get: (path, opts) => request('GET', path, opts),
    post: (path, body, opts) => request('POST', path, { ...opts, body }),
    put: (path, body, opts) => request('PUT', path, { ...opts, body }),
    patch: (path, body, opts) => request('PATCH', path, { ...opts, body }),
    del: (path, opts) => request('DELETE', path, opts),
};

export const enc = encodeURIComponent;
