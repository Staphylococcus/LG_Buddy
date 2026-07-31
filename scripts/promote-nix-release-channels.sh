#!/usr/bin/env bash

set -euo pipefail

usage() {
    cat <<'EOF'
Usage: promote-nix-release-channels.sh [options]

Options:
  --tag <tag>               SemVer release tag (defaults to GITHUB_REF_NAME)
  --remote <remote>         Git remote name or URL (default: origin)
  --expected-commit <rev>   Require the tag to resolve to this commit
  --dry-run                 Validate and report without pushing
  --validate-only           Validate and classify the tag without using Git
  -h, --help                Show this help
EOF
}

fail() {
    echo "Release-channel promotion failed: $*" >&2
    exit 1
}

TAG="${GITHUB_REF_NAME:-}"
REMOTE="origin"
EXPECTED_COMMIT=""
DRY_RUN=0
VALIDATE_ONLY=0

while [ "$#" -gt 0 ]; do
    case "$1" in
        --tag)
            [ "$#" -ge 2 ] || fail "--tag requires a value"
            TAG="$2"
            shift 2
            ;;
        --remote)
            [ "$#" -ge 2 ] || fail "--remote requires a value"
            REMOTE="$2"
            shift 2
            ;;
        --expected-commit)
            [ "$#" -ge 2 ] || fail "--expected-commit requires a value"
            EXPECTED_COMMIT="$2"
            shift 2
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        --validate-only)
            VALIDATE_ONLY=1
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            fail "unknown argument: $1"
            ;;
    esac
done

[ -n "$TAG" ] || fail "provide a tag with --tag or GITHUB_REF_NAME"

SEMVER_PATTERN='^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-([0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*))?(\+([0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*))?$'
if [[ ! "$TAG" =~ $SEMVER_PATTERN ]]; then
    fail "tag must be SemVer with a leading v, for example v1.2.3 or v1.2.3-beta.1"
fi

PRERELEASE="${BASH_REMATCH[5]:-}"
if [ -n "$PRERELEASE" ]; then
    IFS='.' read -r -a PRERELEASE_IDENTIFIERS <<< "$PRERELEASE"
    for identifier in "${PRERELEASE_IDENTIFIERS[@]}"; do
        if [[ "$identifier" =~ ^[0-9]+$ && "$identifier" == 0[0-9]* ]]; then
            fail "numeric prerelease identifiers must not contain leading zeroes: $identifier"
        fi
    done
    RELEASE_KIND="prerelease"
    CHANNELS=(nix-prerelease)
else
    RELEASE_KIND="stable"
    CHANNELS=(nix-stable nix-prerelease)
fi

echo "$TAG is a $RELEASE_KIND SemVer release."

if [ "$VALIDATE_ONLY" = "1" ]; then
    exit 0
fi

git rev-parse --is-inside-work-tree >/dev/null 2>&1 ||
    fail "run this command from a Git worktree"
git show-ref --verify --quiet "refs/tags/$TAG" ||
    fail "tag does not exist locally: $TAG"

TAG_COMMIT="$(git rev-parse --verify "${TAG}^{commit}")" ||
    fail "tag does not resolve to a commit: $TAG"

if [ -n "$EXPECTED_COMMIT" ]; then
    RESOLVED_EXPECTED_COMMIT="$(git rev-parse --verify "${EXPECTED_COMMIT}^{commit}")" ||
        fail "expected commit does not resolve to a commit: $EXPECTED_COMMIT"
    if [ "$TAG_COMMIT" != "$RESOLVED_EXPECTED_COMMIT" ]; then
        fail "tag $TAG resolves to $TAG_COMMIT, not expected commit $RESOLVED_EXPECTED_COMMIT"
    fi
fi

declare -a ACTIONS=()
declare -a CURRENT_COMMITS=()

for channel in "${CHANNELS[@]}"; do
    remote_ref="refs/heads/$channel"
    remote_line="$(git ls-remote --heads "$REMOTE" "$remote_ref")" ||
        fail "could not read $remote_ref from remote $REMOTE"

    if [[ "$remote_line" == *$'\n'* ]]; then
        fail "remote $REMOTE returned more than one match for $remote_ref"
    fi

    if [ -z "$remote_line" ]; then
        ACTIONS+=("create")
        CURRENT_COMMITS+=("")
        continue
    fi

    current_commit="${remote_line%%[[:space:]]*}"
    git fetch --quiet --no-tags "$REMOTE" "$remote_ref" ||
        fail "could not fetch $remote_ref from remote $REMOTE"

    if [ "$current_commit" = "$TAG_COMMIT" ]; then
        ACTIONS+=("unchanged")
        CURRENT_COMMITS+=("$current_commit")
    elif git merge-base --is-ancestor "$current_commit" "$TAG_COMMIT"; then
        ACTIONS+=("fast-forward")
        CURRENT_COMMITS+=("$current_commit")
    else
        fail "$channel at $current_commit is not an ancestor of tagged commit $TAG_COMMIT; refusing downgrade or divergent update"
    fi
done

for index in "${!CHANNELS[@]}"; do
    channel="${CHANNELS[$index]}"
    action="${ACTIONS[$index]}"
    current_commit="${CURRENT_COMMITS[$index]}"

    case "$action" in
        create)
            if [ "$DRY_RUN" = "1" ]; then
                echo "Would create $channel at $TAG_COMMIT."
            else
                echo "Will create $channel at $TAG_COMMIT."
            fi
            ;;
        fast-forward)
            if [ "$DRY_RUN" = "1" ]; then
                echo "Would fast-forward $channel from $current_commit to $TAG_COMMIT."
            else
                echo "Will fast-forward $channel from $current_commit to $TAG_COMMIT."
            fi
            ;;
        unchanged)
            echo "$channel already points at $TAG_COMMIT."
            ;;
    esac
done

if [ "$DRY_RUN" = "1" ]; then
    exit 0
fi

declare -a TEMP_REFS=()
declare -a REFSPECS=()

cleanup_temp_refs() {
    for temp_ref in "${TEMP_REFS[@]}"; do
        git update-ref -d "$temp_ref" >/dev/null 2>&1 || true
    done
}
trap cleanup_temp_refs EXIT

for index in "${!CHANNELS[@]}"; do
    if [ "${ACTIONS[$index]}" = "unchanged" ]; then
        continue
    fi

    channel="${CHANNELS[$index]}"
    temp_ref="refs/lg-buddy-release-promotion/$$/$channel"
    git update-ref "$temp_ref" "$TAG_COMMIT"
    TEMP_REFS+=("$temp_ref")
    REFSPECS+=("$temp_ref:refs/heads/$channel")
done

if [ "${#REFSPECS[@]}" -eq 0 ]; then
    echo "No release-channel refs need promotion."
    exit 0
fi

git push --atomic "$REMOTE" "${REFSPECS[@]}"
echo "Promoted ${CHANNELS[*]} to release tag $TAG ($TAG_COMMIT)."
