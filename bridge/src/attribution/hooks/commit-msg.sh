#!/bin/sh
# commit-msg, from the Jesse bridge: refuse a commit that carries AI attribution, then run
# the repository's own commit-msg hook and exit with its status.
. "$(dirname -- "$0")/jesse-hooks-lib.sh"

# The identity git is about to record for role $1, from the environment ($2, $3) and from
# `git var $4`, which also reads the configuration.
jesse_check_identity() {
    if [ -n "$2$3" ]; then
        jesse_identity_hit "$1" "$2 <$3>"
    fi
    if jesse_ident=$(git var "$4" 2>/dev/null); then
        jesse_identity_hit "$1" "${jesse_ident%%>*}>"
    fi
}

jesse_hits=$(
    {
        jesse_message_hits < "$1"
        jesse_check_identity author "${GIT_AUTHOR_NAME-}" "${GIT_AUTHOR_EMAIL-}" GIT_AUTHOR_IDENT
        jesse_check_identity committer "${GIT_COMMITTER_NAME-}" "${GIT_COMMITTER_EMAIL-}" \
            GIT_COMMITTER_IDENT
    } | awk '!seen[$0]++'
)

if [ -n "$jesse_hits" ]; then
    {
        echo "jesse: commit rejected: it carries AI attribution, which is not allowed."
        printf '%s\n' "$jesse_hits" | sed 's/^/    /'
        echo "Remove the line(s) above from the message (and use your own author and"
        echo "committer, not Claude's), then commit again."
    } >&2
    exit 1
fi

if jesse_hook=$(jesse_repo_hook commit-msg); then
    "$jesse_hook" "$@"
    exit $?
fi
exit 0
