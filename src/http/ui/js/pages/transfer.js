// Import & export: download a catalog's .tar.gz, or upload one with the
// CLI's options. An <a href> cannot carry the bearer header, so the export
// is fetched and handed to the browser as a blob: object URL.
import { h, clear } from '../core/dom.js';
import { api, enc } from '../core/api.js';
import { store } from '../core/store.js';
import { toast } from '../core/toast.js';
import { count } from '../core/format.js';
import { fileDrop, upload } from '../components/upload.js';

let exportUrl = null;   // the last export's object URL, released when replaced

function forget() {
    if (exportUrl) URL.revokeObjectURL(exportUrl);
    exportUrl = null;
}

function tile(label, n, id) {
    return h('div', { class: 'tile' }, h('div', { class: 'n', id }, String(n)), h('div', { class: 'l' }, label));
}

function paths(label, list) {
    return list.length
        ? h('details', {}, h('summary', {}, `${label} (${list.length})`), h('ul', { class: 'mono small' }, list.map(p => h('li', {}, p))))
        : null;
}

function findings(list) {
    return list.map(f => h('li', {}, h('code', {}, `${f.file}:${f.line}`), ` ${f.rule}: ${f.message}`));
}

function reportPanel(report) {
    const { errors, warnings } = report.lint;
    return h('div', { id: 'import-report', class: 'panel panel-ok' },
        h('h3', {}, 'Imported'),
        h('div', { class: 'tiles' },
            tile('added', report.added.length, 'import-added'),
            tile('updated', report.updated.length, 'import-updated'),
            tile('deleted', report.deleted.length, 'import-deleted'),
            tile('unchanged', report.unchanged, 'import-unchanged')),
        paths('Added', report.added), paths('Updated', report.updated), paths('Deleted', report.deleted),
        h('p', { id: 'import-lint' }, `Lint: ${count(errors.length, 'error')}, ${count(warnings.length, 'warning')}`),
        errors.length || warnings.length ? h('ul', { class: 'small' }, findings(errors), findings(warnings)) : null);
}

function errorPanel(error) {
    const message = error.status === 413
        ? 'The file is larger than the server accepts (max_import_bytes).'
        : error.message;
    const problems = error.body?.problems ?? [];
    return h('div', { id: 'import-error', class: 'panel panel-error' },
        h('h3', {}, error.status ? `Refused (HTTP ${error.status})` : 'Failed'),
        h('p', {}, message),
        problems.length ? h('ul', { id: 'import-problems', class: 'mono small' }, problems.map(p => h('li', {}, p))) : null);
}

export async function render(main, rest) {
    forget();
    let catalogs;
    try { catalogs = (await api.get('/catalogs')).catalogs; } catch { return; }
    const known = name => catalogs.some(c => c.name === name);
    let chosen = known(rest[0]) ? rest[0] : known(store.catalog) ? store.catalog : catalogs[0]?.name;
    if (!chosen) {
        main.append(h('h1', {}, 'Import & export'), h('p', { class: 'muted' }, 'This key reaches no catalog.'));
        return;
    }
    let file = null;
    const current = () => catalogs.find(c => c.name === chosen);

    const picker = h('select', { id: 'transfer-catalog' }, catalogs.map(c => h('option', { value: c.name }, c.name)));
    picker.value = chosen;
    const generationLabel = h('span', { id: 'transfer-generation', class: 'mono small muted' });
    const generation = h('input', { type: 'number', id: 'import-generation', min: '0', step: '1' });
    const showGeneration = () => {
        generationLabel.textContent = `generation ${current().generation}`;
        generation.value = String(current().generation);
    };

    // Export
    const exportOut = h('span', { id: 'export-out' });
    const exportButton = h('button', { type: 'button', id: 'export-btn' }, 'Export .tar.gz');
    exportButton.addEventListener('click', async () => {
        exportButton.disabled = true;
        try {
            const response = await api.get(`/catalogs/${enc(chosen)}/export`, { raw: true });
            const blob = await response.blob();
            // Navigated away meanwhile: hand nothing over, and leave the
            // object URL slot to the newer render.
            if (!main.isConnected) return;
            forget();
            exportUrl = URL.createObjectURL(blob);
            const link = h('a', { id: 'export-link', href: exportUrl, download: `${chosen}.tar.gz`,
                                  dataset: { size: String(blob.size) } }, `Save ${chosen}.tar.gz again`);
            exportOut.replaceChildren(link, h('span', { class: 'muted small' }, ` ${blob.size} bytes`));
            link.click();
        } catch { /* toasted */ } finally {
            exportButton.disabled = false;
        }
    });

    // Import (admin only; the server checks anyway)
    const result = h('div', { id: 'import-result' });
    const progress = h('progress', { id: 'import-progress', max: '100', value: '0', hidden: true });
    const replace = h('input', { type: 'checkbox', id: 'import-replace' });
    const machinery = h('input', { type: 'checkbox', id: 'import-machinery' });
    machinery.addEventListener('change', () => { if (machinery.checked) replace.checked = true; });
    const importButton = h('button', { type: 'button', id: 'import-btn', disabled: true }, 'Import');
    const drop = fileDrop({
        accept: '.tar.gz,.tgz,application/gzip',
        label: 'Drop a .tar.gz here, or choose one.',
        onFile: chosenFile => { file = chosenFile; importButton.disabled = false; clear(result); },
    });
    importButton.addEventListener('click', async () => {
        if (!file) return;
        const query = new URLSearchParams();
        if (replace.checked) query.set('replace', 'true');
        if (machinery.checked) query.set('only_machinery', 'true');
        if (generation.value.trim() !== '') query.set('expect_generation', generation.value.trim());
        importButton.disabled = true;
        progress.hidden = false;
        progress.value = 0;
        clear(result);
        try {
            const report = await upload(`/catalogs/${enc(chosen)}/import?${query}`, file,
                fraction => { progress.value = Math.round(fraction * 100); });
            result.append(reportPanel(report));
            toast(`Imported into ${chosen}.`, 'ok');
            try { catalogs = (await api.get('/catalogs')).catalogs; showGeneration(); } catch { /* toasted */ }
        } catch (error) {
            result.append(errorPanel(error));
            toast(error.status === 0 ? error.message : 'The import was refused.', 'error');
        } finally {
            importButton.disabled = false;
            progress.hidden = true;
        }
    });

    picker.addEventListener('change', () => {
        chosen = picker.value;
        showGeneration();
        clear(result);
        exportOut.replaceChildren();
        forget();
    });
    showGeneration();

    main.append(
        h('div', { class: 'page-head' },
            h('div', {}, h('h1', {}, 'Import & export'),
                h('p', { class: 'muted' }, 'Move a catalog in and out as a .tar.gz, as okf-mcp import and export do.'))),
        h('label', { class: 'catalog-pick' }, 'Catalog ', picker, ' ', generationLabel),
        h('section', { class: 'panel', id: 'export-panel' },
            h('h2', {}, 'Export'),
            h('p', { class: 'muted small' }, 'The published documents, as the catalog stands now.'),
            exportButton, ' ', exportOut),
        store.can('admin')
            ? h('section', { class: 'panel', id: 'import-panel' },
                h('h2', {}, 'Import'),
                drop,
                h('div', { class: 'options' },
                    h('label', {}, replace, ' Replace the catalog\'s contents'),
                    h('label', {}, machinery, ' Only machinery (AGENTS.md, tools/, ...)'),
                    h('label', {}, 'Expected generation ', generation)),
                h('p', { class: 'muted small' }, 'The import is refused if the catalog has moved past the expected generation; clear the field to skip the check.'),
                importButton, progress, result)
            : null);
}
