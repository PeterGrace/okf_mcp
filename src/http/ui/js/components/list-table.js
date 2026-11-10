import { h } from '../core/dom.js';

// A table over rows already in memory, sortable by any column with a sort().
// columns: [{ key, label, render?(row) -> Node|string, sort?(row) -> value, class? }]
export function listTable(columns, rows, { onRow, empty = 'Nothing here.' } = {}) {
    let sortKey = null;
    let direction = 1;
    let filter = null;
    const body = h('tbody');
    const head = h('tr', {}, columns.map(column => h('th', {
        scope: 'col',
        class: column.sort ? 'sortable' : null,
        onclick: column.sort ? () => { direction = sortKey === column.key ? -direction : 1; sortKey = column.key; draw(); } : null,
    }, column.label)));
    const table = h('table', { class: 'list-table' }, h('thead', {}, head), body);

    function draw() {
        const view = filter ? rows.filter(filter) : rows.slice();
        if (sortKey) {
            const column = columns.find(c => c.key === sortKey);
            view.sort((a, b) => {
                const x = column.sort(a);
                const y = column.sort(b);
                return (x > y ? 1 : x < y ? -1 : 0) * direction;
            });
        }
        body.replaceChildren(...(view.length
            ? view.map(row => h('tr', { class: onRow ? 'clickable' : null, onclick: onRow ? () => onRow(row) : null },
                columns.map(column => h('td', { class: column.class ?? null },
                    column.render ? column.render(row) : (row[column.key] ?? '-')))))
            : [h('tr', {}, h('td', { colspan: columns.length, class: 'muted empty' }, empty))]));
    }

    table.redraw = newRows => { if (newRows) rows = newRows; draw(); };
    table.setFilter = fn => { filter = fn; draw(); };
    draw();
    return table;
}
