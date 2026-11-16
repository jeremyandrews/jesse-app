#!/bin/sh
# pre-push, from the Jesse bridge: refuse a push of any commit that carries AI attribution,
# then run the repository's own pre-push hook (same arguments, same stdin) and exit with its
# status.
. "$(dirname -- "$0")/jesse-hooks-lib.sh"

jesse_remote=$1
jesse_in=$(mktemp "${TMPDIR:-/tmp}/jesse-pre-push.XXXXXX") || exit 1
trap 'rm -f "$jesse_in"' EXIT
cat > "$jesse_in"

# Whether $1 is an all-zero object name (a ref that does not exist on one side).
jesse_is_zero() {
    case $1 in
        *[!0]*) return 1 ;;
        *) return 0 ;;
    esac
}

# The commits one pushed ref ($1 local sha, $2 remote sha) sends.
jesse_pushed_commits() {
    if ! jesse_is_zero "$2" && git cat-file -e "$2^{commit}" 2>/dev/null; then
        git rev-list "$2..$1"
    elif [ -n "$(git for-each-ref --count=1 --format=x "refs/remotes/$jesse_remote/")" ]; then
        # A new branch (or a remote tip this clone lacks): what the remote does not have yet,
        # as far as this clone knows it, which spares its default branch.
        git rev-list "$1" --not --remotes="$jesse_remote"
    else
        git rev-list "$1" --not --remotes
    fi
}

jesse_scan() {
    while read -r jesse_lref jesse_lsha jesse_rref jesse_rsha; do
        [ -n "$jesse_lsha" ] || continue
        # A deletion pushes no commit.
        jesse_is_zero "$jesse_lsha" && continue
        for jesse_c in $(jesse_pushed_commits "$jesse_lsha" "$jesse_rsha"); do
            jesse_hits=$(jesse_commit_hits "$jesse_c")
            if [ -n "$jesse_hits" ]; then
                printf '  %s %s (%s)\n' "$(git rev-parse --short=12 "$jesse_c")" \
                    "$(git log -1 --format=%s "$jesse_c")" "$jesse_rref"
                printf '%s\n' "$jesse_hits" | sed 's/^/      /'
            fi
        done
    done < "$jesse_in"
}

jesse_report=$(jesse_scan)
if [ -n "$jesse_report" ]; then
    {
        echo "jesse: push rejected: these commits carry AI attribution, which is not allowed:"
        printf '%s\n' "$jesse_report"
        echo "Remove the attribution from each commit listed (message lines, and any Claude"
        echo "author or committer), then push again."
    } >&2
    exit 1
fi

if jesse_hook=$(jesse_repo_hook pre-push); then
    "$jesse_hook" "$@" < "$jesse_in"
    exit $?
fi
exit 0
