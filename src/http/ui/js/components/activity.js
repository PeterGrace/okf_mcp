// Console sessions and sign-in history as tables: the Account page shows
// the signed-in person's (Task 14b), a user's drawer (Task 15) any user's.
import { h, pill } from '../core/dom.js';
import { ago, when } from '../core/format.js';
import { guard, once } from '../core/pending.js';
import { listTable } from './list-table.js';

const OUTCOME_KIND = { ok: 'ok', failed: 'warn', locked: 'error' };

// sessions: [{id_prefix, created_at, last_seen_at, ip, user_agent, current?}].
// Each row but the current one has a Sign out button calling revoke(row);
// with `revokeCurrent`, the current one has one too (the Account page signs
// this browser out with it). A sign-out is the pending write
// `<scope>:<id_prefix>` (core/pending.js) until the promise revoke returns
// settles: its button, and any drawn again meanwhile (a re-sort), stays
// disabled, so it is never sent twice. `scope` (default `prefix`) names
// whose sessions these are. `prefix` starts its data-test
// (`<prefix>-<id_prefix>`, `<prefix>-revoke-<id_prefix>`), and `allowed`
// false disables it.
export function sessionTable({ id, sessions, prefix, revoke, allowed = true, revokeCurrent = false, scope = prefix }) {
    const table = listTable([
        { key: 'user_agent', label: 'Browser',
          render: row => h('span', { class: 'small', dataset: { test: `${prefix}-${row.id_prefix}` } },
              row.user_agent || 'unknown browser', row.current ? [' ', pill('this browser', 'info')] : null) },
        { key: 'ip', label: 'Address', class: 'mono', render: row => row.ip ?? '-' },
        { key: 'created_at', label: 'Signed in', sort: row => row.created_at, render: row => when(row.created_at) },
        { key: 'last_seen_at', label: 'Last seen', sort: row => row.last_seen_at, render: row => ago(row.last_seen_at) },
        { key: 'actions', label: '',
          render: row => {
              if (row.current && !revokeCurrent) return '-';
              const key = `${scope}:${row.id_prefix}`;
              return guard(h('button', {
                  type: 'button', class: 'text-btn',
                  dataset: { test: `${prefix}-revoke-${row.id_prefix}` },
                  onclick: () => once(key, () => revoke(row)),
              }, 'Sign out'), key, allowed);
          } },
    ], sessions, { empty: 'No console sessions.' });
    table.id = id;
    return table;
}

// signIns: [{method, outcome, ip, at}], newest first.
export function signInTable({ id, signIns }) {
    const table = listTable([
        { key: 'at', label: 'When', sort: row => row.at, render: row => when(row.at) },
        { key: 'method', label: 'Method' },
        { key: 'outcome', label: 'Outcome', render: row => pill(row.outcome, OUTCOME_KIND[row.outcome]) },
        { key: 'ip', label: 'Address', class: 'mono', render: row => row.ip ?? '-' },
    ], signIns, { empty: 'No sign-ins yet.' });
    table.id = id;
    return table;
}
