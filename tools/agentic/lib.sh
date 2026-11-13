#!/usr/bin/env bash
# Shared helpers for the agentic-development scripts in this directory.
# Sourced, not run: `source "$(dirname "$0")/lib.sh"`.

# The GitHub repository (owner/name) of the current checkout, or $AGENTIC_REPO.
agentic_repo() {
    if [ -n "${AGENTIC_REPO:-}" ]; then
        printf '%s\n' "$AGENTIC_REPO"
    else
        gh repo view --json nameWithOwner --jq .nameWithOwner
    fi
}

# The login prefix of the review bot (matched with startswith, so the "[bot]"
# suffix GitHub adds does not matter). Override with $AGENTIC_REVIEWER.
agentic_reviewer() {
    printf '%s\n' "${AGENTIC_REVIEWER:-brooke}"
}

# The directory shared by every worktree of this repository (git's common dir),
# where cross-worktree state such as the gate lock lives.
agentic_state_dir() {
    local common
    common=$(git rev-parse --path-format=absolute --git-common-dir) || return 1
    mkdir -p "$common/agentic"
    printf '%s\n' "$common/agentic"
}

# The top of the main checkout (the parent of the common .git directory).
agentic_main_checkout() {
    local common
    common=$(git rev-parse --path-format=absolute --git-common-dir) || return 1
    dirname "$common"
}
