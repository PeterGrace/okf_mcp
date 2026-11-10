# Light ontology for OKF catalogs: design

Status: idea, written up 2026-11-10. Not scheduled; no implementation plan yet.

## Summary

Give a catalog an optional, advisory schema: a short list of canonical concept
types with aliases for the old spellings, and a small vocabulary of typed
relations that concepts may declare in frontmatter. The server normalizes
types when it reads, adds relations to the link graph, shows each relation's
inverse beside the backlinks, and lints against the schema with **warnings
only**. Nothing is ever refused for a schema reason, no relation is ever
required, and the prose stays the source of truth.

This is deliberately the light version. A strict one (required keys per type,
publish refusals, mandatory relations) is a non-goal; see "Why not stricter".

## Problem

OKF concepts are markdown files with YAML frontmatter. Today the only
structure between them is an untyped markdown link, the folder tree, and a
free-text `type`. A scan of a production catalog on 2026-11-09 (259
documents, 1,350 concept-to-concept links, 1,205 distinct linked pairs)
found:

1. **`type` has drifted.** 40 distinct values across about 245 concepts; 25
   of them are used once or twice, with near-synonyms (several names for a
   machine, several for a running piece of software, two for a place).
   Filtering `search` by `type` misses most of what it should find.
2. **Links are mostly typeable.** A hand-labelled random sample of 100
   links found that about 89% express one of nine relations (part of,
   depends on or uses, runs on, owned or created by, about, tracked in,
   detail in, playbook for, follows). Only about 11% are a real untyped "see
   also".
3. **60% of linked pairs are linked in both directions by hand** (728 of
   1,205). Each such pair is a fact written twice that can drift. The worst
   cases are the biggest hubs, whose hand-kept lists of "what runs here" or
   "what is tracked here" are a main cause of their sprawl.

| Relation implied (sample of 100) | Share |
|---|---|
| owned or created by, people | 16% |
| about, tracked in (backlog and playbook to subject) | 15% |
| depends on or uses | 14% |
| part of or contains (including location) | 13% |
| untyped "see also" | 11% |
| incident and playbook relations | 10% |
| detail in (hub to elaborating concept) | 9% |
| runs on | 8% |
| repository hosted on | 4% |

## Goals

- Make `type` usable as a filter without rewriting any document.
- Let a concept say *how* it relates to another, where the author knows.
- Show a concept the inverse of every relation pointing at it, so hubs no
  longer need hand-kept back-reference lists.
- Tell writers, gently, when they use a type or relation the schema does not
  know.
- Change nothing for a catalog that has no schema.

## Non-goals

- **Refusing a write for a schema reason.** Schema findings are lint
  warnings, never errors, so `publish` never fails because of them.
- **Required relations or required keys per type.** Required structure
  invites agents to invent edges to satisfy the validator.
- **Replacing body links.** Untyped links remain legal and normal.
- **Injecting generated text into concept bodies.** Inverses are data in
  tool responses, not text written into files.
- **Bulk migration.** No changeset rewrites types or adds relations across
  the catalog.
- **Formal semantics** (OWL-style classes, inheritance, inference, cardinality).
- **Mapping between catalogs' schemas.**

## Design

### 1. The schema document

A catalog may have one schema at `/SCHEMA.md`. Its body holds a fenced code
block with the info string `okf-schema`, written in YAML; prose around it
explains the choices to human and agent readers. The block lives in the body,
not frontmatter, so it is edited with `edit_document` like any other text,
goes through changesets, and has `history` and `revert`.

```okf-schema
version: 1
types:
  Host:
    description: A physical or virtual machine.
    aliases: [NAS, Switch, Device, Asset]
  Service:
    description: Software that runs somewhere and is used by something.
    aliases: [System, Subsystem, Component]
  Place:
    aliases: [Location]
  Project:
    aliases: [Project Idea]
relations:
  part_of:
    inverse: contains
    description: This concept is a component, child or member of the target.
  runs_on:
    inverse: runs
    targets: [Host, Service]
  depends_on:
    inverse: used_by
  owned_by:
    inverse: owns
  about:
    inverse: discussed_in
  tracked_in:
    inverse: tracks
  playbook_for:
    inverse: playbooks
  follows:
    inverse: followed_by
```

Rules:

- `SCHEMA.md` becomes a machinery document (`DocKind`), like `index.md` and
  `log.md`: `read_document` reads it, it is not a concept, and `search` does
  not return it.
- A schema that does not parse is a lint **error** on `SCHEMA.md` itself (so
  a broken schema cannot be published), and the catalog is then treated as
  having no schema. This is the only error the feature adds.
- Type names and aliases are unique, matched case-insensitively. Relation
  names are snake_case and unique, inverses included.
- `targets` is optional. Without it, a relation may point at any concept.

### 2. Type normalization on read

When a snapshot loads, each concept's `type` is resolved to a **canonical
type**: itself if it is a schema type, the type it is an alias of, or none.

- `search` with `type` matches on the canonical type, so `type: Host` finds
  concepts written `NAS`.
- `get_concept` returns the raw `type` unchanged and adds `canonical_type`.
- Files are never rewritten. An author who touches a concept may change its
  `type` to the canonical name; lint suggests it.

### 3. Relations in frontmatter

A new optional top-level key `relations`, placed after `tags` in `KEY_ORDER`:

```yaml
relations:
  runs_on: [/infra/cluster.md]
  depends_on:
    - /infra/object-store.md
    - path: /infra/registry.md
      note: image pulls only
```

- Each value is a list of concept paths (absolute, as links are written), or
  of `{path, note}` maps. The `note` carries the context a bare edge loses.
- New write tools, mirroring `add_source` and `remove_source`:
  `relate(path, relation, target, note?)` and `unrelate(path, relation, target)`.
  They maintain the key's formatting, so writers never rebuild the
  frontmatter by hand. `set_frontmatter` stays refused for `relations`, as it
  is for `sources`.
- Relation targets join the link graph:
  - they appear in `get_concept` `links` (with the relation name) and in the
    target's backlinks;
  - `move_concept` and `split_concept` rewrite them as they rewrite body links;
  - `delete_concept` counts them in `StillLinked`;
  - a target that does not exist is a broken link, with the severity body
    links already have.
- Relations are optional everywhere. A concept with no `relations` key is
  complete.

### 4. Inverses

`get_concept` with `links: true` adds `inverse_relations`: for each relation
pointing at this concept, its inverse name with the source concepts and their
notes, grouped by name. An agent reading a hub can then answer "what runs
here?" without the hub listing it.

Hubs may drop their hand-kept back-reference lists once the matching
relations exist. That is an ordinary editorial change, made one hub at a time
by whoever judges the generated view good enough, not an automatic step.

### 5. Lint (warnings only)

New lint warnings, each marked "(not in okf.py)" in the README like the
server's other extensions:

| Code | When |
|---|---|
| `schema-type-alias` | `type` is an alias; names the canonical type |
| `schema-type-unknown` | `type` is neither a type nor an alias; names the closest types |
| `schema-relation-unknown` | `relations` uses a name the schema does not declare |
| `schema-relation-target-type` | a relation's target has a canonical type outside its `targets` |
| `schema-relation-self` | a concept relates to itself |

Without a schema none of these run. Because they are warnings, `publish`
returns them in `warnings` and never refuses. `lint` reports the counts per
code, so a maintenance run can track them over time.

### 6. Telling agents about the schema

- The catalog's `AGENTS.md` should say the schema exists and where; the
  catalog template should carry an example `SCHEMA.md`.
- `create_concept`'s result includes any schema warnings for the new
  concept, so the writer can fix them before publishing.
- Tool JSON schemas stay static (they are shared across catalogs), so the
  allowed types are not an enum in `create_concept`'s input schema.

### 7. Maintenance runner (later, optional)

The runner's `suggested_link` finding could carry an optional `relation`, so
the investigator can propose "add `runs_on: /x.md`, because the body says it
is deployed there". Accepting the finding applies it with `relate`. This is a
follow-up, not part of the first slice.

## Compatibility

- **A catalog without `SCHEMA.md` behaves exactly as today**: no new
  warnings, no `canonical_type`, and `relations` is just an unknown key with
  no special meaning.
- **`okf.py`**: the Python tool does not know `relations` or `SCHEMA.md`. A
  catalog exported and linted by `okf.py` keeps working; it just does not see
  relation edges as links. Lint output differs from `okf.py` only by the new
  warnings, which the README marks as extensions.
- **Import and export** carry `SCHEMA.md` and `relations` as ordinary file
  content.

## Why not stricter

Making structure mandatory has costs that outweigh the gains for a catalog
written mostly by agents under constant change:

- **Writes cost more, so fewer marginal facts get recorded.** The catalog's
  value depends on cheap writes.
- **Validators get gamed.** Agents satisfy required fields with plausible
  but wrong values (a catch-all type, an invented `depends_on`). A wrong
  typed edge misleads more than a missing one, because tools trust it.
- **Structure goes stale and gets trusted over prose.** Hence: the prose
  stays authoritative, and relations are an index into it.
- **Refusals add retries.** Every new refusal reason is a re-read and retry
  for some session. Warnings do not.
- **The schema itself needs care.** Keeping it small and advisory keeps that
  care cheap; a wrong rule produces warnings, not blocked work.

If the light version proves itself, individual rules can be promoted later
(for example, `schema-type-unknown` to an error on *new* concepts only),
each as its own decision.

## Rollout

1. Ship the schema document, type normalization and the type lint warnings.
   Seed a catalog's `SCHEMA.md` with canonical types and aliases covering its
   existing values. Benefit: `type` filters work at once, with no document
   rewritten.
2. Ship `relations`, `relate`/`unrelate`, link-graph integration and
   `inverse_relations`. Writers start adding relations where they know them.
3. As coverage of a relation grows, editors retire hand-kept lists in hubs.
4. Optionally, relation suggestions from the maintenance runner.

There is no backfill step and no migration changeset. Coverage grows as
concepts are touched, which puts it first where the traffic is.

## Work breakdown (if scheduled)

| Slice | Contents |
|---|---|
| A | `SCHEMA.md` as a machinery document; the `okf-schema` parser and its error; tests |
| B | Canonical type resolution in the snapshot; `search` `type` filter; `canonical_type` in `get_concept` |
| C | Type lint warnings and per-code counts |
| D | `relations` parsing; `relate` / `unrelate`; `KEY_ORDER` |
| E | Relations in the link graph: links, backlinks, `move_concept`, `split_concept`, `delete_concept`, broken targets |
| F | `inverse_relations` in `get_concept`; relation lint warnings |
| G | README, catalog template example, release |

A to C stand alone and give most of the type benefit; D to F depend on A.

## Open questions

- Should `relations` accept relative paths, as body links do, or only
  absolute ones? Absolute is simpler to rewrite and to read in frontmatter.
- Is `note` worth having in the first slice, or should it wait for evidence
  that bare edges lose too much context?
- Should the inverse view also be available as a section in the web
  console's concept page?
- Should `schema-type-alias` be quieter (counted in the report but not
  repeated on every publish) to avoid warning fatigue on old concepts?
