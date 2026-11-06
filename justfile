# Common local validation commands. Keep these recipes as the stable interface
# used by contributors and issue-to-PR automation.

fmt:
    cargo fmt --all --check

lint:
    cargo clippy --all-targets -- -D warnings
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps

test:
    cargo test
