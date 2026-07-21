#!/bin/sh
set -eu

repo_root=${1:-$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)}
case "$repo_root" in
    /*) ;;
    *) repo_root=$(CDPATH= cd -- "$repo_root" && pwd) ;;
esac

overall=0

pass() {
    echo "PASS: $*"
}

fail() {
    echo "FAIL: $*" >&2
    overall=1
}

require_trace_match() {
    trace=$1
    description=$2
    pattern=$3
    if printf '%s\n' "$trace" | grep -Fq "$pattern"; then
        pass "$description"
    else
        fail "$description"
    fi
}

require_trace_absent() {
    trace=$1
    description=$2
    pattern=$3
    if printf '%s\n' "$trace" | grep -Fq "$pattern"; then
        fail "$description"
    else
        pass "$description"
    fi
}

if awk '
    /^[[:space:]]*\.PHONY[[:space:]]*:/ {
        declaration = $0
        sub(/^[^:]*:/, "", declaration)
        count = split(declaration, targets, /[[:space:]]+/)
        for (item = 1; item <= count; item++) {
            if (targets[item] == "build") {
                found = 1
            }
        }
    }
    END { exit(found ? 0 : 1) }
' "$repo_root/Makefile"; then
    pass 'root build target is phony'
else
    fail 'root build target must be phony'
fi

if awk '
    /^build[[:space:]]*:/ {
        found = 1
        line = $0
        sub(/^[^:]*:/, "", line)
        sub(/^[[:space:]]*/, "", line)
        valid = (line == "")
        exit
    }
    END { exit(found && valid ? 0 : 1) }
' "$repo_root/Makefile"; then
    pass 'root build target has no prerequisites'
else
    fail 'root build target must have no prerequisites'
fi

build_recipe=$(awk '
    /^[^[:space:]#][^:]*:/ {
        if (in_rule) {
            exit
        }
        if ($0 ~ /^build[[:space:]]*:/) {
            in_rule = 1
        }
        next
    }
    in_rule && /^[[:space:]]*\t/ { print }
' "$repo_root/Makefile")
if [ "$build_recipe" = '	$(MAKE) -C os all' ]; then
    pass 'root build target has exactly one os-all recipe'
else
    fail 'root build target must have exactly one os-all recipe'
fi

if build_trace=$(make -C "$repo_root" -n -j8 build 2>&1); then
    delegation_count=$(printf '%s\n' "$build_trace" | grep -Fc 'make -C os all')
    if [ "$delegation_count" -eq 1 ]; then
        pass 'root build delegates to os all exactly once under -j8'
    else
        fail 'root build must delegate to os all exactly once under -j8'
    fi
    require_trace_absent "$build_trace" 'root build does not provision Rustup' 'scripts/rustup-setup.sh'
    require_trace_absent "$build_trace" 'root build does not rerun root preparation' 'make prepare-cargo-config'
else
    fail 'root build target must be available to make -n'
fi

if all_trace=$(make -C "$repo_root" -n -j8 all 2>&1); then
    setup_line=$(printf '%s\n' "$all_trace" | awk '/scripts\/rustup-setup\.sh/ { print NR; exit }')
    prepare_line=$(printf '%s\n' "$all_trace" | awk '/make prepare-cargo-config/ { print NR; exit }')
    os_all_line=$(printf '%s\n' "$all_trace" | awk '/make -C os all/ { print NR; exit }')
    if [ -n "$setup_line" ] && [ -n "$prepare_line" ] && [ -n "$os_all_line" ] \
        && [ "$setup_line" -lt "$prepare_line" ] && [ "$prepare_line" -lt "$os_all_line" ]; then
        pass 'root all retains setup, preparation, and os-all order under -j8'
    else
        fail 'root all must retain setup, preparation, and os-all order under -j8'
    fi
else
    fail 'root all must be available to make -n -j8'
fi

exit "$overall"
