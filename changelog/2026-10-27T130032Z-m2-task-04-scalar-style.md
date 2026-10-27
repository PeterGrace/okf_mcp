# M2 Task 4: `yaml::scalar` takes a `ScalarStyle` (2026-10-27T13:00:32Z)

Issue #6, Task 4 of `docs/superpowers/plans/2026-10-27-okf-mcp-m2-analysis.md`. An M1
follow-up: `scalar(text, flow, force)` took two positional bools, so a call such as
`scalar(x, false, true)` didn't say what it meant. The M1 final review asked for an enum.

## Added

- `ScalarStyle` in `src/okf/yaml.rs`: `Block` (quoted only when needed), `Flow`
  (also quoted when `,[]{}` would be flow syntax) and `Quoted` (always double-quoted).
- Test `yaml::tests::scalar_styles`.

## Changed

- `scalar(text: &str, style: ScalarStyle) -> String` replaces `scalar(text, flow, force)`.
  `flow_value` passes `Flow`, `block_item` passes `Block`; the doc example uses
  `Block` and `Quoted`.
- `src/okf/doc.rs` test helpers: `quoted_item` uses `Quoted` (was `false, true`), the
  `set_block` property uses `Block` (was `false, false`). The Hegel property
  `scalar_round_trips_through_parse_inline` uses `Block`.

## Notes

- Rendering is unchanged: `render_matches_golden` and every existing render test pass.
- Implemented as the plan's Task 4 specifies; no plan correction was needed. No later
  task in the M2 plan calls `scalar`, so none needed updating. The M1 plan still shows the
  old signature; it is a completed plan and was left as written.
- `needs_quote(text, flow)` keeps its bool: it is one meaningful flag, and `scalar` is
  its only non-test caller.

## Verification

- `cargo fmt --check`: clean.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo test`: 141 unit, 4 parity (5 ignored), 15 doc tests passed.
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`: clean.
