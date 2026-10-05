# One unprivileged shell owns the native authorization subject and flow lease.
# Requests and responses are framed, never interpreted as shell code.
# shellcheck source-path=SCRIPTDIR
set -uo pipefail
_lg_buddy_mode="$1"
exec 3>&1 4<&0
_lg_buddy_mutation=0
_lg_buddy_begin_mutation() {
    [ "${_lg_buddy_cancellable:-0}" = 1 ] || return 0
    [ "$_lg_buddy_mutation" = 0 ] || return 0
    printf 'mutation\n' >&3
    local answer
    IFS= read -r -d '' answer <&4 || return 126
    [ "$answer" = continue ] || return 126
    _lg_buddy_mutation=1
}
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
        terminal)
            if [ "${_lg_buddy_cancellable:-0}" = 1 ]; then
                /usr/bin/sudo -v || return 127
                _lg_buddy_begin_mutation || return $?
                /usr/bin/sudo -n "$@"
            else /usr/bin/sudo "$@"; fi
            return $? ;;
        noninteractive)
            if [ "${_lg_buddy_cancellable:-0}" = 1 ]; then
                /usr/bin/sudo -n /usr/bin/true || return 127
                _lg_buddy_begin_mutation || return $?
            fi
            /usr/bin/sudo -n "$@"; return $? ;;
        interactive) ;;
        *) return 1 ;;
    esac
    # Polkit checks the real grant on every use, including its expiry. Never
    # retry a dismissed/denied challenge or fall back to another authenticator.
    if /usr/bin/pkcheck --action-id io.github.staphylococcus.LGBuddy.setup \
        --process "$_lg_buddy_subject" --allow-user-interaction >/dev/null; then
        _lg_buddy_begin_mutation || return $?
        /usr/bin/pkexec --disable-internal-agent "$@"
    else
        local status=$?
        [ "$status" -ne 3 ] || return 126
        return 127
    fi
}

# KWin's native worker owns its state and lock. This owner keeps the existing
# authorization subject and cancellation handshake for each privileged request.
_lg_buddy_kwin_privileged() {
    local helper="$1"
    shift
    if [ "$EUID" -eq 0 ]; then
        _lg_buddy_begin_mutation || return $?
        /bin/bash "$helper" "$@"
    elif [ -x /usr/bin/sudo ] && /usr/bin/sudo -n /usr/bin/true 2>/dev/null; then
        _lg_buddy_begin_mutation || return $?
        /usr/bin/sudo -n /bin/bash "$helper" "$@"
    elif [ "$_lg_buddy_mode" = noninteractive ]; then
        return 127
    elif [ "$_lg_buddy_mode" = terminal ]; then
        _lg_buddy_privileged /bin/bash "$helper" "$@"
    else
        _lg_buddy_privileged "$helper" "$@"
    fi
}

_lg_buddy_kwin() {
    local runtime="$1" payload="$2"
    shift 2
    exec 5>&1 6>&2
    # Hold even a no-op worker until its coprocess descriptors are retained.
    # Bash otherwise unsets them if a fast worker exits before the next command.
    coproc _lg_buddy_native {
        IFS= read -r -d '' _lg_buddy_start || exit 1
        exec "$runtime" kwin-setup --broker --payload-dir "$payload" "$@"
    }
    local pid="$_lg_buddy_native_PID" input output operation count argument status i
    local original_input="${_lg_buddy_native[1]}" original_output="${_lg_buddy_native[0]}"
    exec {input}>&"$original_input" {output}<&"$original_output"
    exec {original_input}>&- {original_output}<&-
    printf '\0' >&"$input"
    local -a arguments
    while IFS= read -r -d '' operation <&"$output"; do
        IFS= read -r -d '' count <&"$output" || break
        [[ "$count" =~ ^[0-9]+$ ]] && [ "$count" -le 16 ] || break
        arguments=()
        for (( i=0; i<count; i++ )); do
            IFS= read -r -d '' argument <&"$output" || break 2
            arguments+=("$argument")
        done
        status=0
        case "$operation" in
            mutation) _lg_buddy_begin_mutation || status=$? ;;
            privileged) _lg_buddy_kwin_privileged "$payload/setup.sh" "${arguments[@]}" || status=$? ;;
            *) break ;;
        esac
        printf '%s\0' "$status" >&"$input" || break
    done
    exec {input}>&- {output}<&- 5>&- 6>&-
    status=0
    wait "$pid" || status=$?
    return "$status"
}

_lg_buddy_run() {
    case "$_lg_buddy_operation" in
        services)
            _lg_buddy_privileged /usr/lib/lg-buddy/setup-services "$_lg_buddy_path"
            ;;
        plasma)
            # The compatibility launcher starts the native worker through this
            # owner, preserving the same subject for privileged requests.
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
    _lg_buddy_cancellable=0
    case "$_lg_buddy_operation" in *-cancellable) _lg_buddy_cancellable=1; _lg_buddy_operation="${_lg_buddy_operation%-cancellable}" ;; esac
    _lg_buddy_mutation=0
    if _lg_buddy_run </dev/null >"$_lg_buddy_output/stdout" 2>"$_lg_buddy_output/stderr"; then
        _lg_buddy_status=0
    else
        _lg_buddy_status=$?
    fi
    printf '%s\n%s\n%s\n' "$_lg_buddy_status" \
        "$(wc -c <"$_lg_buddy_output/stdout")" "$(wc -c <"$_lg_buddy_output/stderr")"
    cat -- "$_lg_buddy_output/stdout" "$_lg_buddy_output/stderr"
done
