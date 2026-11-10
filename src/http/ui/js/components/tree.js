// The catalog tree for Browse, and the console route of a document.
// Folders are native <details> elements, so they collapse without script.
// documents: [{ path: '/a/b.md', kind: 'concept'|'index'|'log'|'machinery', title }]
import { h } from '../core/dom.js';
import { browseHash } from '../core/router.js';

const KIND_ORDER = { index: 0, concept: 1, log: 2, machinery: 3 };

function label(doc) {
    if (doc.kind === 'index') return 'Overview';
    return doc.title ?? doc.path.split('/').pop();
}

// selected: the '/'-prefixed path of the open document; the folders above it start open.
export function tree(documents, { catalog, selected }) {
    const root = { folders: new Map(), files: [] };
    for (const doc of documents) {
        const names = doc.path.replace(/^\//, '').split('/');
        let node = root;
        for (const name of names.slice(0, -1)) {
            if (!node.folders.has(name)) node.folders.set(name, { folders: new Map(), files: [] });
            node = node.folders.get(name);
        }
        node.files.push(doc);
    }
    return h('div', { class: 'tree', id: 'catalog-tree' }, branch(root, '', catalog, selected));
}

function branch(node, prefix, catalog, selected) {
    const folders = [...node.folders.keys()].sort().map(name => {
        const path = prefix + name;
        return h('li', {},
            h('details', { class: 'tree-folder', dataset: { folder: path }, open: selected.startsWith(`/${path}/`) },
                h('summary', {}, name),
                branch(node.folders.get(name), `${path}/`, catalog, selected)));
    });
    const files = node.files
        .slice()
        .sort((a, b) => (KIND_ORDER[a.kind] - KIND_ORDER[b.kind]) || label(a).localeCompare(label(b)))
        .map(doc => h('li', {},
            h('a', {
                href: browseHash(catalog, doc.path),
                class: doc.path === selected ? 'tree-link active' : 'tree-link',
                title: doc.path,
                dataset: { path: doc.path },
            }, label(doc))));
    return h('ul', {}, folders, files);
}
