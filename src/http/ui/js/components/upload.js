// File choice by picker or drag and drop, and an upload with progress.
// fetch() cannot report upload progress, so the upload uses XMLHttpRequest
// with the same bearer header api.js sends.
import { h } from '../core/dom.js';
import { store } from '../core/store.js';
import { api, ApiError } from '../core/api.js';

// A drop zone around a file input. onFile(file) runs on every choice.
export function fileDrop({ accept, label, onFile }) {
    const input = h('input', { type: 'file', id: 'upload-file', accept });
    const name = h('span', { id: 'upload-name', class: 'mono small muted' }, 'No file chosen');
    const zone = h('div', { id: 'upload-drop', class: 'upload-drop' }, h('p', {}, label), input, name);
    const choose = file => {
        if (!file) return;
        name.textContent = `${file.name} (${file.size} bytes)`;
        onFile(file);
    };
    input.addEventListener('change', () => choose(input.files[0]));
    zone.addEventListener('dragover', event => { event.preventDefault(); zone.classList.add('over'); });
    zone.addEventListener('dragleave', () => zone.classList.remove('over'));
    zone.addEventListener('drop', event => {
        event.preventDefault();
        zone.classList.remove('over');
        choose(event.dataTransfer.files[0]);
    });
    return zone;
}

// POSTs `file` as application/gzip to /api/v1<path>. onProgress(fraction)
// follows the upload. Resolves with the JSON answer of a 2xx; rejects with
// an ApiError otherwise. The caller shows it: nothing is toasted here.
export function upload(path, file, onProgress = () => {}) {
    return new Promise((resolve, reject) => {
        const xhr = new XMLHttpRequest();
        xhr.open('POST', `/api/v1${path}`);
        xhr.setRequestHeader('Authorization', `Bearer ${store.key ?? ''}`);
        xhr.setRequestHeader('Content-Type', 'application/gzip');
        xhr.responseType = 'json';   // a non-JSON body (413) arrives as null
        xhr.upload.addEventListener('progress', event => {
            if (event.lengthComputable) onProgress(event.loaded / event.total);
        });
        xhr.addEventListener('load', () => {
            if (xhr.status >= 200 && xhr.status < 300) {
                onProgress(1);
                resolve(xhr.response);
                return;
            }
            // A refused key: let api.js's shared 401 path sign the browser out.
            if (xhr.status === 401) api.get('/whoami').catch(() => {});
            reject(new ApiError(xhr.status, xhr.response));
        });
        xhr.addEventListener('error', () =>
            reject(new ApiError(0, { message: 'The upload failed: the server could not be reached.' })));
        xhr.send(file);
    });
}
