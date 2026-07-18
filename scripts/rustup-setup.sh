#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
manifest="$script_dir/../rust-toolchain.toml"

if [ -z "${RUSTUP_HOME:-}" ]; then
    echo "RUSTUP_HOME must be set and non-empty; setup is explicit and does not use HOME fallback" >&2
    exit 1
fi

if [ ! -r "$manifest" ]; then
    echo "$manifest: missing or unreadable rust-toolchain.toml; setup is explicit" >&2
    exit 1
fi

manifest_values=$(awk '
function trim(value) {
    sub(/^[[:space:]]+/, "", value)
    sub(/[[:space:]]+$/, "", value)
    return value
}
function fail(field) {
    printf "%s: malformed or missing %s; setup is explicit\n", FILENAME, field > "/dev/stderr"
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

set --
for component in $components; do
    set -- "$@" --component "$component"
done
for target in $targets; do
    set -- "$@" --target "$target"
done

rustup toolchain install "$toolchain" --profile minimal "$@"

exec sh "$script_dir/rustup-preflight.sh"
