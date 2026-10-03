# One unprivileged shell owns the native authorization subject and flow lease.
# Requests and responses are framed, never interpreted as shell code.
# shellcheck source-path=SCRIPTDIR
set -uo pipefail
_lg_buddy_mode="$1"
umask 077
_lg_buddy_output=$(mktemp -d "${TMPDIR:-/tmp}/lg-buddy-authorization.XXXXXX") || exit 1
trap 'rm -rf -- "$_lg_buddy_output"' EXIT
# A vanished frontend closes the response pipe after the helper finishes.
trap 'exit 0' PIPE
_lg_buddy_stat=$(<"/proc/$$/stat")
read -r -a _lg_buddy_fields <<< "${_lg_buddy_stat##*) }"
_lg_buddy_subject="$$,${_lg_buddy_fields[19]},$UID"

_lg_buddy_privileged() {
    case "$_lg_buddy_mode" in
        terminal) /usr/bin/sudo "$@"; return $? ;;
        noninteractive) /usr/bin/sudo -n "$@"; return $? ;;
        interactive) ;;
        *) return 1 ;;
    esac
    # Polkit checks the real grant on every use, including its expiry. Never
    # retry a dismissed/denied challenge or fall back to another authenticator.
    if /usr/bin/pkcheck --action-id io.github.staphylococcus.LGBuddy.setup \
        --process "$_lg_buddy_subject" --allow-user-interaction >/dev/null; then
        /usr/bin/pkexec --disable-internal-agent "$@"
    else
        local status=$?
        [ "$status" -ne 3 ] || return 126
        return 127
    fi
}

_lg_buddy_run() {
    case "$_lg_buddy_operation" in
        services)
            _lg_buddy_privileged /usr/lib/lg-buddy/setup-services "$_lg_buddy_path"
            ;;
        plasma)
            # The existing helper is sourceable. Running main in this shell
            # keeps every privileged call attached to the same native subject.
            # shellcheck source=../../../../data/kwin/setup.sh
            source "$_lg_buddy_path" || return 1
            local -a args=(--foreground)
            case "$_lg_buddy_mode" in
                terminal) args+=(--terminal) ;;
                noninteractive) args+=(--noninteractive) ;;
                interactive) ;;
                *) return 1 ;;
            esac
            [ "$_lg_buddy_option" != 1 ] || args+=(--allow-dependencies)
            main "${args[@]}"
            ;;
        *) return 1 ;;
    esac
}

while IFS= read -r -d '' _lg_buddy_operation \
    && IFS= read -r -d '' _lg_buddy_path \
    && IFS= read -r -d '' _lg_buddy_option; do
    if _lg_buddy_run </dev/null >"$_lg_buddy_output/stdout" 2>"$_lg_buddy_output/stderr"; then
        _lg_buddy_status=0
    else
        _lg_buddy_status=$?
    fi
    # KWin's step-local lock must not survive until the next request.
    exec 9>&-
    printf '%s\n%s\n%s\n' "$_lg_buddy_status" \
        "$(wc -c <"$_lg_buddy_output/stdout")" "$(wc -c <"$_lg_buddy_output/stderr")"
    cat -- "$_lg_buddy_output/stdout" "$_lg_buddy_output/stderr"
done
