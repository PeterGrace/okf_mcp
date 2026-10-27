# M2 Task 2: Python-semantics helpers in `okf::pystr` (2026-10-27T12:41:41Z)

Issue #4, Task 2 of `docs/superpowers/plans/2026-10-27-okf-mcp-m2-analysis.md`. Lint
messages embed Python `repr()` of values, `oldest` uses `str()`, lint tests
truthiness, unlinked-mention patterns use `re.escape` with `re.I`, and anchor
fragments go through `urllib.parse.unquote`. These helpers give them one home.

## Added

- `pystr::py_repr_str`: Python's `repr()` of a `str` (quote choice, backslash
  escapes, `\xNN`/`\uNNNN`/`\UNNNNNNNN` for non-printable characters per
  `str.isprintable`).
- `pystr::py_repr`, `py_repr_opt`, `py_str`: `repr()`/`str()` of a `yaml::Value`
  (`None`, `'text'`, `[...]`, `{...}` in source key order).
- `pystr::py_truthy`: `bool(d.get(key))`.
- `pystr::ignorecase_literal`: `re.escape` for a `(?i:...)` group, with `i`, `I`,
  U+0130 and U+0131 spelled as one class (verified fact 6).
- `pystr::py_unquote`: `urllib.parse.unquote` (UTF-8, `errors="replace"`), borrowing
  when there is no `%`.

## Changed

- Every `YamlError` message in `yaml.rs` and `doc.rs` quotes source text with
  `py_repr_str` instead of Rust's `{:?}`, so `frontmatter does not parse` messages
  match `okf.py` byte for byte (`'a'` not `"a"`, `é` not `\u{e9}`). Ten sites, not the
  plan's eight: the flow-map `duplicate key` and `unsupported YAML indicator` were
  missing from its table.

## Notes

- Plan corrected with a dated `Corrected (issue #4)` note under Task 2's Step 5 table
  (two added rows). No later task uses a changed symbol; the helper names and
  signatures are as planned.
- Every expected string was checked against Python 3.14.4: `repr`/`unquote`/`re.I`
  directly, and the parse-error messages by loading `okf.py` and calling
  `Doc(path).parse()` on the same frontmatter.
- Tests: five `pystr` tests from the plan; `doc.rs`
  `parse_errors_quote_like_python_repr` (asserts the full flow-list message rather than
  the plan's suffix check); `yaml.rs` `flow_errors_quote_like_python_repr` (flow-map
  duplicate key, ASCII and non-ASCII, and the indicator message).

## Verification

- `cargo fmt --check`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo test`: 130 unit, 5 parity (5 ignored live-parity tests), 14 doc tests passed.
- `cargo test --test parity -- --ignored`: 5 passed.
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`: clean.

## Review follow-up (PR #18)

- Parity compares full parse-error messages: `error_kind` (which cut each message at
  its first quote, because M1 quoted with Rust `Debug`) is removed from
  `tests/parity.rs` and `tests/parity/okf_shim.py`. Both now emit the whole message
  as `error_message`, and `tests/golden/frontmatter.json` was regenerated from
  `okf.py` (`UPDATE_GOLDEN=1`, `SOURCE` unchanged). Each of the 19 corpus files
  that fail to parse (under `errors/` and `people/`) now checks the message byte for byte, which is the regression guard for
  this task's claim. The plan gains a second dated `Corrected (issue #4)` note.
- `repr_and_str_of_values_follow_python` also covers `py_str` of a list and a mapping
  and `py_repr_opt(Some(..))`.
- Not changed: `py_repr_str` still asks a regex about each non-ASCII character. Its
  callers are error and lint messages over short frontmatter values, so a
  general-category table would add code without a measurable gain.
- Gates after the follow-up: 130 unit, 4 parity (5 ignored), 14 doc tests; live
  parity 5 passed. (The parity count drops by one: `error_kind`'s own test went with
  it.)
