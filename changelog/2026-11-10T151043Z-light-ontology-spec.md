# Light ontology design spec

## Added

- `docs/superpowers/specs/2026-11-10-okf-light-ontology-design.md`: a design, not
  scheduled, for an optional, advisory schema for OKF catalogs. It covers:
  - a `SCHEMA.md` holding canonical concept types with aliases, and a small
    vocabulary of typed relations;
  - type normalization when the server reads;
  - an optional `relations` frontmatter key with `relate` and `unrelate` tools,
    joined to the link graph;
  - inverse relations in `get_concept`;
  - lint warnings that never refuse a publish.

## Notes

- The motivation comes from a scan of a production catalog on 2026-11-09: 40
  distinct `type` values (25 used once or twice), about 89% of a sample of links
  typeable with nine relations, and 60% of linked pairs linked both ways by hand.
- Stricter options (required relations, publish refusals) are deliberately left out.
  The spec's "Why not stricter" section gives the reasons.
