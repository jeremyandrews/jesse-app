#!/bin/sh
# @HOOK@, from the Jesse bridge: a pass-through to the repository's own @HOOK@ hook.
. "$(dirname -- "$0")/jesse-hooks-lib.sh"
if jesse_hook=$(jesse_repo_hook @HOOK@); then
    exec "$jesse_hook" "$@"
fi
exit 0
