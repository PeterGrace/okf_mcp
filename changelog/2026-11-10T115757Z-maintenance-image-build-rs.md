# Maintenance image: copy build.rs

## Fixed

- `Dockerfile.maintenance` now copies `build.rs` into the build stage. Since the web
  console (M9), `build.rs` generates the console's asset table, which
  `src/http/ui.rs` includes from `OUT_DIR`. The maintenance image copied only
  `Cargo.toml`, `Cargo.lock`, `src` and `migrations`, so its v0.12.0 build failed
  with "environment variable `OUT_DIR` not defined" and "cannot find value
  `ASSETS`". The server image copies the whole tree and was unaffected.

## Changed

- `scripts/test_maintenance_image.py` now requires `build.rs` among the build
  stage's copied inputs.

## Notes

- Verified with `python3 -B -m unittest discover -s scripts -p 'test_maintenance_image.py'`
  (11 tests, 4 skipped) and a local `docker build -f Dockerfile.maintenance --target build .`.
- The published `v0.12.0` tag is never rebuilt; the maintenance image ships with
  the next release.
