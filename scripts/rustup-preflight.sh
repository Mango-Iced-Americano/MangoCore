#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
manifest="$script_dir/../rust-toolchain.toml"
rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"
missing=0

if [ ! -r "$manifest" ]; then
    echo "$manifest: missing or unreadable rust-toolchain.toml; this read-only command does not provision" >&2
    exit 1
fi

manifest_values=$(awk '
function trim(value) {
    sub(/^[[:space:]]+/, "", value)
    sub(/[[:space:]]+$/, "", value)
    return value
}
function fail(field) {
    printf "%s: malformed or missing %s; this read-only command does not provision\n", FILENAME, field > "/dev/stderr"
    exit 1
}
function parse_scalar(line, field, value) {
    value = line
    sub(/^[^=]*=[[:space:]]*/, "", value)
    if (value !~ /^"[^"]*"[[:space:]]*$/) {
        fail(field)
    }
    sub(/^"/, "", value)
    sub(/"[[:space:]]*$/, "", value)
    if (value == "") {
        fail(field)
    }
    return value
}
function parse_array(line, field, value, inner, count, item, i) {
    value = line
    sub(/^[^=]*=[[:space:]]*/, "", value)
    if (value !~ /^\[[^]]*\][[:space:]]*$/) {
        fail(field)
    }
    sub(/^\[/, "", value)
    sub(/\][[:space:]]*$/, "", value)
    if (trim(value) == "") {
        fail(field)
    }
    count = split(value, inner, ",")
    for (i = 1; i <= count; i++) {
        item = trim(inner[i])
        if (item !~ /^"[^"]*"$/) {
            fail(field)
        }
        sub(/^"/, "", item)
        sub(/"$/, "", item)
        if (item == "") {
            fail(field)
        }
        printf "%s\t%s\n", field, item
    }
}
{
    line = $0
    sub(/[[:space:]]*#.*/, "", line)
    line = trim(line)
    if (line == "") {
        next
    }
    if (line == "[toolchain]") {
        if (section) {
            fail("toolchain section")
        }
        section = 1
        next
    }
    if (!section) {
        fail("toolchain section")
    }
    if (line ~ /^[[:space:]]*channel[[:space:]]*=/) {
        if (channel_seen++) {
            fail("channel")
        }
        printf "channel\t%s\n", parse_scalar(line, "channel")
    } else if (line ~ /^[[:space:]]*targets[[:space:]]*=/) {
        if (targets_seen++) {
            fail("targets")
        }
        parse_array(line, "target")
        targets_present = 1
    } else if (line ~ /^[[:space:]]*components[[:space:]]*=/) {
        if (components_seen++) {
            fail("components")
        }
        parse_array(line, "component")
        components_present = 1
    } else {
        fail("toolchain fields")
    }
}
END {
    if (!section) fail("toolchain section")
    if (!channel_seen) fail("channel")
    if (!targets_seen || !targets_present) fail("targets")
    if (!components_seen || !components_present) fail("components")
}
' "$manifest")

toolchain=""
targets=""
components=""
while IFS="$(printf '\t')" read -r kind value; do
    case "$kind" in
        channel) toolchain=$value ;;
        target) targets="$targets${targets:+ }$value" ;;
        component) components="$components${components:+ }$value" ;;
    esac
done <<EOF
$manifest_values
EOF

toolchain_dir=""
for candidate in "$rustup_home"/toolchains/"$toolchain"-*; do
    if [ -d "$candidate" ]; then
        toolchain_dir="$candidate"
        break
    fi
done

if [ -z "$toolchain_dir" ]; then
    echo "missing Rust toolchain: $toolchain" >&2
    missing=1
else
    for target in $targets; do
        if [ ! -d "$toolchain_dir/lib/rustlib/$target/lib" ]; then
            echo "missing Rust target for $toolchain: $target" >&2
            missing=1
        fi
    done

    components_file="$toolchain_dir/lib/rustlib/components"
    for component in $components; do
        if [ ! -r "$components_file" ] || ! grep -Eq "^${component}(-[^[:space:]]+)?$" "$components_file"; then
            echo "missing Rust component for $toolchain: $component" >&2
            missing=1
        fi
    done
fi

if [ "$missing" -ne 0 ]; then
    echo "this read-only command does not provision; run 'make toolchain-setup' inside the Docker development container to provision the pinned toolchain" >&2
    exit 1
fi

echo "Rust toolchain preflight passed: $toolchain"
