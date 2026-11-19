# The Jesse bridge's git hooks: the functions every hook shares. Sourced, never run.
#
# The bridge writes this directory from its own binary at every start, so an edit here does
# not last. Every process the bridge spawns for a run reaches it through GIT_CONFIG_COUNT /
# GIT_CONFIG_KEY_n=core.hooksPath, and each hook here then hands over to the repository's
# own hook of the same name, so husky, pre-commit and lint hooks keep working.

JESSE_TRAILER_RE='^[[:space:]]*(co-authored-by|claude-session)[[:space:]]*:.*(claude|anthropic)'
JESSE_GEN_RE='generated with .*(claude|anthropic)'
JESSE_GEN_AI_RE='generated with (.*[^[:alnum:]])?ai([^[:alnum:]]|$)'

# This hooks directory, resolved.
jesse_self_dir() {
    CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd -P
}

# The hooks directory the repository uses on its own: its core.hooksPath (local, then global,
# then system), else <common git dir>/hooks.
jesse_repo_hooks_dir() {
    for jesse_scope in --local --global --system; do
        jesse_d=$(git config "$jesse_scope" --type=path --get core.hooksPath 2>/dev/null)
        if [ -n "$jesse_d" ]; then
            printf '%s\n' "$jesse_d"
            return 0
        fi
    done
    jesse_d=$(git rev-parse --git-common-dir 2>/dev/null) || return 1
    printf '%s/hooks\n' "$jesse_d"
}

# The repository's own executable hook named $1, or a failure when it has none. Never this
# directory again, so a repository pointed here does not loop.
jesse_repo_hook() {
    jesse_dir=$(jesse_repo_hooks_dir) || return 1
    jesse_real=$(CDPATH= cd -- "$jesse_dir" 2>/dev/null && pwd -P) || return 1
    [ "$jesse_real" = "$(jesse_self_dir)" ] && return 1
    [ -f "$jesse_real/$1" ] && [ -x "$jesse_real/$1" ] || return 1
    printf '%s\n' "$jesse_real/$1"
}

# The lines of the message on stdin that carry attribution. Comment lines and everything
# under a scissors line (git commit -v) are not part of the message.
jesse_message_hits() {
    awk '/^# -+ >8 -+$/ { exit } /^#/ { next } { print }' |
        LC_ALL=C grep -i -E -e "$JESSE_TRAILER_RE" -e "$JESSE_GEN_RE" -e "$JESSE_GEN_AI_RE"
}

# A line when the identity $2 ("Name <email>") is Claude's or Anthropic's; $1 names its role.
jesse_identity_hit() {
    jesse_name=${2%% <*}
    jesse_email=${2#*<}
    jesse_email=${jesse_email%%>*}
    if printf '%s\n' "$jesse_email" | LC_ALL=C grep -q -i 'anthropic\.com'; then
        printf '%s: %s\n' "$1" "$2"
        return 0
    fi
    if [ "$(printf '%s' "$jesse_name" | LC_ALL=C tr '[:upper:]' '[:lower:]')" = claude ]; then
        printf '%s: %s\n' "$1" "$2"
    fi
    return 0
}

# Every attribution in commit $1: message lines, then its author and committer.
jesse_commit_hits() {
    git log -1 --format=%B "$1" | jesse_message_hits
    jesse_identity_hit author "$(git log -1 --format='%an <%ae>' "$1")"
    jesse_identity_hit committer "$(git log -1 --format='%cn <%ce>' "$1")"
}
