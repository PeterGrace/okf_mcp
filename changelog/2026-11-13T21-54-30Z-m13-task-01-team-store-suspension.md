# M13 Task 1: migration 0011 and the team store's suspension (issue #363)

## Added

- `migrations/0011_team_suspension.sql`: `teams.suspended_at` (null while
  active) and `teams.suspension_notice` (at most 2000 characters, and only
  with a suspension; both are schema constraints).
- `store::teams::MAX_NOTICE_CHARS`, `Suspension` (`since`, `notice`) and
  `SuspendedCatalog` (`team`, `notice`).
- `store::teams::suspend`: suspends a team or replaces the notice of one
  already suspended, keeping the first `suspended_at`; trims the notice,
  stores a blank one as none, refuses one over 2000 characters or holding
  a NUL (Postgres text cannot store NUL, so it would otherwise be a 500);
  bumps the administrative generation.
- `store::teams::unsuspend`: lifts a suspension; a no-op that does not bump
  the generation when the team is active.
- `store::teams::suspended_catalogs`: catalog name to `{team, notice}` for
  every catalog whose owner team is suspended (the key cache loads it in
  Task 3).
- Tests: the five named in the plan, plus a check that the schema refuses a
  notice without a suspension or over the limit.

## Changed

- `TeamRecord` gains `suspension: Option<Suspension>`, read by `list` and
  `find`. `TeamView`'s conversion ignores it until Task 5 shows it.

## Notes

- Both writes lock the team row `FOR UPDATE` like every other team write,
  so a suspend and a lift of one team run one at a time.
- The notice length is counted in characters (`chars().count()`), matching
  the schema's `char_length`, so a multibyte notice is not refused early (tested with 2000
  two-byte characters).
- Plan corrected in place (Task 1 Step 4 note; Task 5's `From<TeamRecord>`
  step now says to change `suspension: _` to `suspension`).
