# M12 Task 2: per-catalog levels, self-issued keys, key expiry

## Added
- `Principal::level(catalog)`; `Reach::Catalogs` maps each catalog to its level.
- `store::keys::CatalogGrant`; `NewKey::{self_issued, expires_at, levels}`;
  `KeyRecord::{catalog_access, expires_at, user_id, self_issued}`.
- `GET /api/v1/keys` shows `catalog_access`, `expires_at` and `self_issued`
  (never `user_id`).

## Changed
- `Principal::require` checks the level of the catalog it names, and its
  `Forbidden` reason names that catalog.
- The key cache leaves out expired keys and grants, and `Keys::find` and
  `find_grant` refuse one that expired since the last reload. `Keys` holds
  the server clock (`Keys::new(cache, clock)`, `Keys::load(pool, clock)`), and
  `KeyCache::from_records`, `find` and `find_grant` take `now`.
- `store::keys::create_with_id` refuses a self-issued key granted every
  catalog, admin or reporting (422 naming the rule), levels on an admin key,
  with every catalog, for a catalog not granted or above the key's access
  (409), and an expiry already past (409). It stores `self_issued`,
  `expires_at` and each grant's level in one statement.
- `store::keys::set_maintenance_report_*` (and so `PATCH /api/v1/keys/{id}`
  and `key maintenance-report`) refuses turning reporting on for a
  self-issued key (422, `maintenance_report`) instead of failing the schema's
  CHECK as a 500.

## Notes
- Every existing key keeps its meaning: NULL levels are the key's own.
- Admin-issued keys get no expiry or levels in this milestone; every
  existing `NewKey` literal passes `self_issued: false, expires_at: None,
  levels: &[]`.
