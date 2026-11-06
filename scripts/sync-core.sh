#!/bin/sh
# Keep vendor/localsend in sync with the official LocalSend Rust core.
#
#   ./scripts/sync-core.sh                 # copy from $LOCALSEND_CORE or a sibling checkout
#   ./scripts/sync-core.sh /path/to/core   # copy from any packages/core checkout
#   ./scripts/sync-core.sh --check         # fail if the vendor copy drifted
#   ./scripts/sync-core.sh --check --require
#   ./scripts/sync-core.sh --from-git https://github.com/localsend/localsend <rev>
#
# Source lookup order: the argument, $LOCALSEND_CORE (a packages/core path or a
# monorepo root), ../localsend/packages/core, ../packages/core.
#
# The vendored copy is what production builds compile: it makes the app
# self-contained and pins a reviewed protocol revision. The revision is kept in
# vendor/localsend/UPSTREAM.md; `--check` verifies it against the source
# repository's HEAD, so a stale pin fails instead of passing silently.
set -eu
cd "$(dirname "$0")/.."
vendor="vendor/localsend"
upstream="$vendor/UPSTREAM.md"

# Temporary directories made by this run (removed on any exit).
tmp_dirs=""

cleanup() {
    if [ -n "$tmp_dirs" ]; then
        # Word splitting is intended: the list holds one path per entry.
        rm -rf $tmp_dirs
    fi
}

trap 'cleanup' EXIT
trap 'cleanup; exit 1' INT TERM HUP

usage() {
    cat <<'EOF'
Keep vendor/localsend in sync with the official LocalSend Rust core.

Usage:
  ./scripts/sync-core.sh [source]              copy packages/core into vendor/
  ./scripts/sync-core.sh --check [source]      fail if vendor/ drifted
  ./scripts/sync-core.sh --from-git <repo> <rev>
  ./scripts/sync-core.sh --require ...         fail (not warn) if no source is found

Source lookup: the argument, $LOCALSEND_CORE, ../localsend/packages/core,
../packages/core.

--check compares the vendored tree (excluding LICENSE, UPSTREAM.md, target and
.git) and verifies the Revision recorded in vendor/localsend/UPSTREAM.md
against the source repository's HEAD. --require turns a missing source
checkout (which skips the comparison) into a hard error.
EOF
    exit "${1:-0}"
}

die() {
    echo "$*" >&2
    exit 1
}

# The commit a source checkout is at, when it is a git working tree.
git_rev() {
    command -v git >/dev/null 2>&1 || return 1
    git -C "$1" rev-parse --verify HEAD 2>/dev/null
}

# The revision recorded in vendor/localsend/UPSTREAM.md, if any.
recorded_revision() {
    [ -f "$upstream" ] || return 1
    sed -n 's/^- Revision: `\([0-9a-f][0-9a-f]*\)`.*/\1/p' "$upstream" | head -1
}

# The packages/core checkout to use, or a failure (no output).
resolve_source() {
    if [ -n "${1:-}" ]; then
        printf '%s\n' "$1"
        return 0
    fi
    if [ -n "${LOCALSEND_CORE:-}" ]; then
        # Accept either packages/core itself or the monorepo root.
        if [ -f "$LOCALSEND_CORE/packages/core/Cargo.toml" ]; then
            printf '%s\n' "$LOCALSEND_CORE/packages/core"
        else
            printf '%s\n' "$LOCALSEND_CORE"
        fi
        return 0
    fi
    for candidate in ../localsend/packages/core ../packages/core; do
        if [ -f "$candidate/Cargo.toml" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

# Copies `source` over vendor/ without ever leaving a half-written tree:
# the new copy is staged in a temporary directory and swapped in last.
copy_from() {
    src="$1"
    [ -d "$src" ] || die "not a directory: $src"
    [ -f "$src/Cargo.toml" ] || die "not a Rust package (no Cargo.toml): $src"

    stage=$(mktemp -d "${TMPDIR:-/tmp}/localsend-core.XXXXXX")
    tmp_dirs="$tmp_dirs $stage"
    mkdir -p "$stage/new"
    # Two commands, not a pipe: `set -e` then catches a failing `tar -c` as
    # well (`/bin/sh` has no pipefail, and `tar -x` of an empty archive
    # "succeeds").
    tar -cf "$stage/core.tar" -C "$src" --exclude target --exclude .git .
    tar -xf "$stage/core.tar" -C "$stage/new"

    # The upstream package has no LICENSE file of its own; the vendored copy
    # carries the repository's Apache-2.0 text. Keep the hand-maintained
    # provenance and license files across syncs.
    if [ -f "$vendor/LICENSE" ]; then
        cp "$vendor/LICENSE" "$stage/new/LICENSE"
    fi
    if [ -f "$upstream" ]; then
        cp "$upstream" "$stage/new/UPSTREAM.md"
    fi

    # Swap: keep the old tree until the new one is in place, and put it back
    # if the move fails.
    old="$vendor.old.$$"
    rm -rf "$old"
    if [ -d "$vendor" ]; then
        mv "$vendor" "$old"
    fi
    if ! mv "$stage/new" "$vendor"; then
        rm -rf "$vendor"
        if [ -d "$old" ]; then
            mv "$old" "$vendor"
        fi
        die "could not replace $vendor; the previous copy was restored"
    fi
    rm -rf "$old"

    source_rev=$(git_rev "$src" || true)
    if [ -n "$source_rev" ]; then
        echo "vendored $src -> $vendor (source HEAD $(printf '%s' "$source_rev" | cut -c1-12))"
        pinned=$(recorded_revision || true)
        if [ -n "$pinned" ] && [ "$pinned" != "$source_rev" ]; then
            if git -C "$src" rev-parse --verify "$pinned^{commit}" >/dev/null 2>&1 \
                && git -C "$src" diff --quiet "$pinned" -- . 2>/dev/null; then
                echo "revision unchanged: packages/core is byte-identical to $pinned"
            else
                echo "WARNING: $upstream pins $pinned" >&2
                echo "         packages/core changed; review the new core and update the Revision" >&2
                echo "         line to $source_rev before committing." >&2
            fi
        fi
    else
        echo "vendored $src -> $vendor (source is not a git checkout)"
    fi
    echo "run ./scripts/check.sh to verify the vendored copy"
}

# Fails when the vendor copy, or the revision UPSTREAM.md records, drifted.
check() {
    src=$(resolve_source "${1:-}") || {
        if [ "$require" -eq 1 ]; then
            echo "ERROR: no LocalSend core checkout found; cannot verify vendor drift." >&2
            echo "       Set LOCALSEND_CORE to a packages/core checkout or pass its path." >&2
            exit 1
        fi
        echo "WARNING: no LocalSend core checkout found; vendor drift NOT verified." >&2
        echo "         Set LOCALSEND_CORE or pass the packages/core path (--require fails instead)." >&2
        return 0
    }
    [ -d "$src" ] || die "not a directory: $src"
    [ -f "$src/Cargo.toml" ] || die "not a Rust package (no Cargo.toml): $src"

    diff_status=0
    # target/ and .git/ are excluded by copy_from too, so they cannot look
    # like drift.
    diff -r --exclude LICENSE --exclude UPSTREAM.md --exclude target --exclude .git \
        "$src" "$vendor" >/dev/null 2>&1 || diff_status=$?
    if [ "$diff_status" -eq 1 ]; then
        echo "vendor/localsend is out of date with $src" >&2
        echo "run ./scripts/sync-core.sh $src" >&2
        exit 1
    elif [ "$diff_status" -ne 0 ]; then
        echo "could not compare vendor/localsend with $src (diff exit $diff_status)" >&2
        exit 1
    fi

    pinned=$(recorded_revision || true)
    source_rev=$(git_rev "$src" || true)
    if [ -n "$pinned" ] && [ -n "$source_rev" ] && [ "$pinned" != "$source_rev" ]; then
        # A newer HEAD commit may not have touched packages/core, so requiring
        # HEAD == pinned would be a false alarm. Compare the working tree with
        # the pinned revision instead: vendor == working tree was just proven
        # by `diff`, so "no core changes since the pin" makes the pin true.
        if git -C "$src" rev-parse --verify "$pinned^{commit}" >/dev/null 2>&1; then
            if ! git -C "$src" diff --quiet "$pinned" -- . 2>/dev/null; then
                echo "$upstream records revision $pinned," >&2
                echo "but packages/core has changed since then (source HEAD is $source_rev)." >&2
                echo "Re-review the core, update $upstream to the new revision, and commit both." >&2
                exit 1
            fi
            echo "content still matches the pinned revision $pinned (HEAD $source_rev has no core changes)"
        else
            echo "WARNING: recorded revision $pinned is not in $src; cannot verify it against HEAD." >&2
            if [ "$require" -eq 1 ]; then
                exit 1
            fi
        fi
    fi

    echo "vendor/localsend matches $src"
    if [ -n "$pinned" ] && [ -n "$source_rev" ]; then
        echo "recorded revision $pinned verified against $src"
    elif [ -n "$pinned" ]; then
        echo "WARNING: $upstream records $pinned, but $src is not a git checkout;" >&2
        echo "         only the file contents were compared." >&2
    else
        echo "WARNING: $upstream has no machine-readable Revision line" >&2
        if [ "$require" -eq 1 ]; then
            exit 1
        fi
    fi
}

require=0
mode="sync"
source_arg=""
repo=""
rev=""

while [ $# -gt 0 ]; do
    case "$1" in
        --check)
            mode="check"
            shift
            ;;
        --require)
            require=1
            shift
            ;;
        --from-git)
            [ $# -ge 3 ] || die "--from-git needs <repository> <revision>"
            mode="from-git"
            repo="$2"
            rev="$3"
            shift 3
            ;;
        -h|--help)
            usage 0
            ;;
        --)
            shift
            break
            ;;
        -*)
            die "unknown option: $1 (see --help)"
            ;;
        *)
            [ -z "$source_arg" ] || die "only one source path is accepted"
            source_arg="$1"
            shift
            ;;
    esac
done

case "$mode" in
    check)
        check "$source_arg"
        ;;
    from-git)
        tmp=$(mktemp -d "${TMPDIR:-/tmp}/localsend-git.XXXXXX")
        tmp_dirs="$tmp_dirs $tmp"
        git init --quiet "$tmp/repo"
        git -C "$tmp/repo" remote add origin "$repo"
        git -C "$tmp/repo" fetch --quiet --depth 1 origin "$rev"
        git -C "$tmp/repo" sparse-checkout init --cone
        git -C "$tmp/repo" sparse-checkout set packages/core
        git -C "$tmp/repo" checkout --quiet FETCH_HEAD
        copy_from "$tmp/repo/packages/core"
        ;;
    sync)
        if core_source=$(resolve_source "$source_arg"); then
            copy_from "$core_source"
        else
            echo "ERROR: no LocalSend core checkout found." >&2
            echo "       Set LOCALSEND_CORE, pass a path to packages/core, or use" >&2
            echo "       --from-git <repo> <rev>." >&2
            exit 1
        fi
        ;;
esac
