// The browser's WebAuthn API takes and returns ArrayBuffers; the server's
// JSON (webauthn-rs) carries them as unpadded base64url. These convert both
// ways. Shared by the sign-in and setup pages (passkey.js) and the account
// page.

export function toBuffer(base64url) {
    const base64 = base64url.replace(/-/g, '+').replace(/_/g, '/');
    const binary = atob(base64 + '='.repeat((4 - (base64.length % 4)) % 4));
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
    return bytes.buffer;
}

export function toBase64url(buffer) {
    let binary = '';
    for (const byte of new Uint8Array(buffer)) binary += String.fromCharCode(byte);
    return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

// navigator.credentials.create() options from a registration challenge.
export function creationOptions(challenge) {
    const publicKey = structuredClone(challenge.publicKey);
    publicKey.challenge = toBuffer(publicKey.challenge);
    publicKey.user.id = toBuffer(publicKey.user.id);
    publicKey.excludeCredentials = (publicKey.excludeCredentials ?? [])
        .map(credential => ({ ...credential, id: toBuffer(credential.id) }));
    return { publicKey };
}

// navigator.credentials.get() options from a sign-in challenge (no
// mediation: the server asks for a modal prompt).
export function requestOptions(challenge) {
    const publicKey = structuredClone(challenge.publicKey);
    publicKey.challenge = toBuffer(publicKey.challenge);
    publicKey.allowCredentials = (publicKey.allowCredentials ?? [])
        .map(credential => ({ ...credential, id: toBuffer(credential.id) }));
    return { publicKey };
}

// A created credential as webauthn-rs's RegisterPublicKeyCredential. The
// transports and client extension results are left out: both are optional,
// and a browser may report values the server does not know.
export function registrationJson(credential) {
    return {
        id: credential.id,
        rawId: toBase64url(credential.rawId),
        type: credential.type,
        extensions: {},
        response: {
            attestationObject: toBase64url(credential.response.attestationObject),
            clientDataJSON: toBase64url(credential.response.clientDataJSON),
        },
    };
}

// The whole browser side of a registration, for the account page (Task
// 14b): the server's challenge in, the credential JSON out. Rejects with the
// browser's DOMException (NotAllowedError) when the prompt is cancelled.
export async function createCredential(challenge) {
    return registrationJson(await navigator.credentials.create(creationOptions(challenge)));
}

// An assertion as webauthn-rs's PublicKeyCredential.
export function assertionJson(credential) {
    const response = credential.response;
    return {
        id: credential.id,
        rawId: toBase64url(credential.rawId),
        type: credential.type,
        extensions: {},
        response: {
            authenticatorData: toBase64url(response.authenticatorData),
            clientDataJSON: toBase64url(response.clientDataJSON),
            signature: toBase64url(response.signature),
            userHandle: response.userHandle ? toBase64url(response.userHandle) : null,
        },
    };
}
