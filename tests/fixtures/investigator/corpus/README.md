# Investigator structural fixtures

`evaluation.json` is a version-1 synthetic test corpus. It contains no private catalog data, captured provider output, credentials, or human-reviewed live quality labels. Its expected keys and admissibility flags are explicit, hand-computed test specifications authored during implementation, not independent human ground truth. `synthetic_specification` makes that distinction machine-readable. Do not present these scores as model accuracy. **Genuine independent human corpus review remains an operator gate before automation.** Passing synthetic specifications or declaring `human_reviewed` does not satisfy that gate.

## Input contract

`evaluate::run(&Path)` reads one JSON file (at most 16 MiB and 10,000 cases). It does not run inference or use the network. Corpus, case, packet, source, revision pin, relation and annotation DTOs must be JSON objects, not positional arrays. Case metadata fields are flattened scalars, not another DTO. Enum-valued metadata, lanes, relation categories and proposal categories/relationships must be JSON strings; Serde externally tagged object representations are not accepted. DTOs reject unknown and duplicate fields by decoding the original bytes. The proposal is retained as raw JSON and decoded by the same internal strict proposal-only decoder as live Claude output; proposal, finding and evidence objects reject positional arrays and duplicate fields. Malformed proposals contribute no accepted predictions and leave expected keys unmatched; malformed corpus DTOs return a fixed Config error. Provider envelopes and monetary lexemes never enter this evaluator or its shared decoder.

```text
{schema_version: 1, cases: [{
  id: unique nonempty string,
  provenance: synthetic | reviewed_captured_output,
  annotation_author: nonempty declared annotator/specification author,
  review: synthetic_specification | human_reviewed | ambiguous | unreviewed,
  packet: Packet,
  proposal: raw JSON value,
  expected: [{category: Category, paths: [normalized evidence paths]}],
  admissibility: [{key: {category: Category, paths: [...]}, admissible: boolean}]
}]}
```

For captured output, a human must supply expected relation keys and admissibility decisions, identify themselves, and explicitly select `human_reviewed`. The evaluator does not authenticate annotator identity or invent annotations. `synthetic_specification` is refused for captured provenance. Ambiguity is a case-level decision: mark the whole case `ambiguous` when any assertion cannot be adjudicated; do not infer a semantic answer from a citation match. Missing admissibility for any accepted prediction excludes the entire case.

## Metric definitions

Keys are `(category, sorted distinct evidence paths)`, deduplicated within each case on both prediction and expectation sides. Revision numbers and assertion wording are not part of a relation key. Only whole proposals that pass production citation/shape/context validation yield predictions.

- TP: accepted, explicitly admissible keys matching expected keys exactly.
- FP: other accepted keys, including explicitly inadmissible predictions even when their keys appear in expectations.
- FN: expected keys not matched by an admissible accepted prediction. Rejected proposals leave their expected keys unmatched.
- `precision = TP / (TP + FP)`; `recall = TP / (TP + FN)`. Zero denominator means JSON `null`.
- Ambiguous/unreviewed/incompletely annotated cases contribute neither predictions nor expectations to these denominators; `excluded_unreviewed` counts excluded cases.
- `rejected_citations`: invalid evidence entries in decoded proposals (unknown source ID, empty/oversize/non-exact excerpt). An otherwise malformed raw proposal is rejected, not partially repaired.
- `rejected_verification`: raw proposals containing forbidden structured `assessment` or `outcome` fields, including nested attempts hidden in overwritten duplicate fields; diagnostics visit original field occurrences and count once per proposal.
- `false_verification`: accepted submissions whose structured assessment is `supported_current`. Production mapping must keep this zero. This is not a natural-language detector for assertions of currency.
- Structural rejection diagnostics include excluded cases and are not semantic precision/recall.

## Hand-computed totals

| Case | TP | FP | FN | Diagnostic |
|---|---:|---:|---:|---|
| correct-link | 1 | 0 | 0 | exact admissible relation |
| similar-name-distinct | 0 | 1 | 1 | inadmissible prediction cannot match even the same expected key |
| copied-claim | 0 | 1 | 0 | copied text does not justify the suggested link |
| historical-change | 1 | 0 | 0 | potential historical change, not currency certification |
| contradiction | 1 | 0 | 0 | conflicting catalog assertions, not externally resolved |
| aging-unknown | 1 | 0 | 0 | remains unverifiable/inconclusive |
| invalid-citation | 0 | 0 | 1 | one rejected citation |
| current-injection | 0 | 0 | 1 | one blocked verification attempt |
| outcome-injection | 0 | 0 | 0 | one blocked outcome injection |
| ambiguous | excluded | excluded | excluded | explicit case exclusion |
| unreviewed | excluded | excluded | excluded | explicit case exclusion |

Total: 11 cases, TP=4, FP=2, FN=3, rejected citations=1, false verification=0, rejected verification=2, excluded=2. Structural precision=4/6 and recall=4/7. Tests also cover duplicate keys, unknown citations, missing annotations, malformed inputs and each undefined denominator independently. No live-model quality claim follows from these numbers.
