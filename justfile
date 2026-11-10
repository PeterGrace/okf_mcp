# Common local validation commands. Keep these recipes as the stable interface
# used by contributors and issue-to-PR automation.

fmt:
    cargo fmt --all --check

lint:
    cargo clippy --all-targets -- -D warnings
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps

test:
    cargo test

# Browser end-to-end tests of the web console (needs Docker and Chromium;
# RUSTWRIGHT_CHROMIUM names the binary if it is not on PATH).
test-ui:
    cargo test --test ui_e2e -- --test-threads=2

# Prepare locally; ship through a PR, then publish the merged commit's tag.
release-prepare patchlevel:
    bash scripts/release-prepare.sh {{quote(patchlevel)}}

release-ship:
    bash scripts/release-ship.sh
