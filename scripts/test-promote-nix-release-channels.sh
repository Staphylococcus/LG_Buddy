#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROMOTION_SCRIPT="$SCRIPT_DIR/promote-nix-release-channels.sh"
TEST_ROOT="$(mktemp -d)"
REMOTE_REPOSITORY="$TEST_ROOT/remote.git"
WORK_REPOSITORY="$TEST_ROOT/work"

cleanup() {
    if [ -n "${TEST_ROOT:-}" ] && [ "$TEST_ROOT" != "/" ]; then
        rm -rf -- "$TEST_ROOT"
    fi
}
trap cleanup EXIT

fail() {
    echo "Release-channel promotion test failed: $*" >&2
    exit 1
}

remote_ref() {
    git --git-dir="$REMOTE_REPOSITORY" rev-parse --verify "refs/heads/$1"
}

assert_remote_ref() {
    local channel="$1"
    local expected="$2"
    local actual

    actual="$(remote_ref "$channel")" ||
        fail "expected remote branch $channel to exist"
    [ "$actual" = "$expected" ] ||
        fail "expected $channel at $expected, got $actual"
}

assert_remote_ref_absent() {
    local channel="$1"

    if git --git-dir="$REMOTE_REPOSITORY" show-ref --verify --quiet "refs/heads/$channel"; then
        fail "expected remote branch $channel to be absent"
    fi
}

commit_state() {
    local value="$1"
    local message="$2"

    printf '%s\n' "$value" > "$WORK_REPOSITORY/state"
    git -C "$WORK_REPOSITORY" add state
    git -C "$WORK_REPOSITORY" commit --quiet -m "$message"
    git -C "$WORK_REPOSITORY" rev-parse HEAD
}

run_promotion() {
    (
        cd "$WORK_REPOSITORY"
        "$PROMOTION_SCRIPT" --remote origin "$@"
    )
}

git init --quiet --bare "$REMOTE_REPOSITORY"
git init --quiet --initial-branch=main "$WORK_REPOSITORY"
git -C "$WORK_REPOSITORY" config user.name "LG Buddy release test"
git -C "$WORK_REPOSITORY" config user.email "release-test@example.invalid"
git -C "$WORK_REPOSITORY" remote add origin "$REMOTE_REPOSITORY"

BASE_COMMIT="$(commit_state base "base")"
PRERELEASE_ONE_COMMIT="$(commit_state prerelease-one "prerelease one")"
git -C "$WORK_REPOSITORY" tag v1.0.0-beta.1 "$PRERELEASE_ONE_COMMIT"

run_promotion --tag v1.0.0-beta.1 --expected-commit "$PRERELEASE_ONE_COMMIT"
assert_remote_ref nix-prerelease "$PRERELEASE_ONE_COMMIT"
assert_remote_ref_absent nix-stable

STABLE_ONE_COMMIT="$(commit_state stable-one "stable one")"
git -C "$WORK_REPOSITORY" tag --annotate --message "stable one" v1.0.0 "$STABLE_ONE_COMMIT"
STABLE_ONE_TAG_OBJECT="$(git -C "$WORK_REPOSITORY" rev-parse refs/tags/v1.0.0)"

run_promotion --tag v1.0.0 --expected-commit "$STABLE_ONE_TAG_OBJECT"
assert_remote_ref nix-stable "$STABLE_ONE_COMMIT"
assert_remote_ref nix-prerelease "$STABLE_ONE_COMMIT"

PRERELEASE_TWO_COMMIT="$(commit_state prerelease-two "prerelease two")"
git -C "$WORK_REPOSITORY" tag v1.1.0-beta.1 "$PRERELEASE_TWO_COMMIT"

run_promotion --tag v1.1.0-beta.1
assert_remote_ref nix-stable "$STABLE_ONE_COMMIT"
assert_remote_ref nix-prerelease "$PRERELEASE_TWO_COMMIT"

STABLE_TWO_COMMIT="$(commit_state stable-two "stable two")"
git -C "$WORK_REPOSITORY" tag v1.1.0 "$STABLE_TWO_COMMIT"

DRY_RUN_OUTPUT="$(run_promotion --tag v1.1.0 --dry-run)"
grep -F -q "v1.1.0 is a stable SemVer release." <<< "$DRY_RUN_OUTPUT" ||
    fail "stable dry run did not report stable classification"
grep -F -q "Would fast-forward nix-stable" <<< "$DRY_RUN_OUTPUT" ||
    fail "stable dry run did not target nix-stable"
grep -F -q "Would fast-forward nix-prerelease" <<< "$DRY_RUN_OUTPUT" ||
    fail "stable dry run did not target nix-prerelease"
assert_remote_ref nix-stable "$STABLE_ONE_COMMIT"
assert_remote_ref nix-prerelease "$PRERELEASE_TWO_COMMIT"

git -C "$WORK_REPOSITORY" switch --quiet --create stable-with-divergent-prerelease "$STABLE_ONE_COMMIT"
ATOMIC_REFUSAL_COMMIT="$(commit_state stable-with-divergent-prerelease "atomic refusal")"
git -C "$WORK_REPOSITORY" tag v1.0.1 "$ATOMIC_REFUSAL_COMMIT"

if run_promotion --tag v1.0.1 >/dev/null 2>&1; then
    fail "stable promotion unexpectedly accepted a divergent prerelease channel"
fi
assert_remote_ref nix-stable "$STABLE_ONE_COMMIT"
assert_remote_ref nix-prerelease "$PRERELEASE_TWO_COMMIT"
git -C "$WORK_REPOSITORY" switch --quiet main

VALIDATE_OUTPUT="$("$PROMOTION_SCRIPT" --tag v9.0.0-rc.1 --validate-only)"
grep -F -q "prerelease SemVer release" <<< "$VALIDATE_OUTPUT" ||
    fail "validate-only did not report prerelease classification"

if "$PROMOTION_SCRIPT" --tag v1.0 --validate-only >/dev/null 2>&1; then
    fail "invalid SemVer tag unexpectedly passed validation"
fi

if "$PROMOTION_SCRIPT" --tag v1.0.0-01 --validate-only >/dev/null 2>&1; then
    fail "numeric prerelease identifier with a leading zero unexpectedly passed validation"
fi

if run_promotion --tag v1.0.0 >/dev/null 2>&1; then
    fail "older stable release unexpectedly moved the prerelease channel backward"
fi
assert_remote_ref nix-stable "$STABLE_ONE_COMMIT"
assert_remote_ref nix-prerelease "$PRERELEASE_TWO_COMMIT"

git -C "$WORK_REPOSITORY" switch --quiet --create divergent "$BASE_COMMIT"
DIVERGENT_COMMIT="$(commit_state divergent "divergent prerelease")"
git -C "$WORK_REPOSITORY" tag v2.0.0-beta.1 "$DIVERGENT_COMMIT"

if run_promotion --tag v2.0.0-beta.1 >/dev/null 2>&1; then
    fail "divergent prerelease unexpectedly replaced the prerelease channel"
fi
assert_remote_ref nix-prerelease "$PRERELEASE_TWO_COMMIT"

if run_promotion --tag v2.0.0-beta.1 --expected-commit "$BASE_COMMIT" >/dev/null 2>&1; then
    fail "mismatched expected commit unexpectedly passed validation"
fi

echo "Release-channel promotion tests passed."
