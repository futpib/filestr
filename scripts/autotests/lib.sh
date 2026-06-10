# helpers for filestr e2e tests; source from test-*.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="$ROOT/target/debug"
TESTDIR="$(mktemp -d "${TMPDIR:-/tmp}/filestr-e2e.XXXXXX")"
PIDS=()

cleanup() {
    local pid
    for pid in "${PIDS[@]:-}"; do
        kill "$pid" 2>/dev/null || true
    done
    wait 2>/dev/null || true
    rm -rf "$TESTDIR"
}
trap cleanup EXIT

die() {
    echo "FATAL: $*" >&2
    local d
    for d in "$TESTDIR"/*/daemon.log; do
        [ -f "$d" ] || continue
        echo "--- $d (last 30 lines)" >&2
        tail -n 30 "$d" >&2
    done
    exit 1
}

# start_node <name> [--share]
start_node() {
    local name="$1"
    shift
    local dir="$TESTDIR/$name"
    mkdir -p "$dir/data"
    local share_toml=""
    if [ "${1:-}" = "--share" ]; then
        mkdir -p "$dir/share"
        share_toml="$(printf '[[share]]\nname = "files"\npath = "%s"\n' "$dir/share")"
    fi
    cat > "$dir/config.toml" <<EOF
socket = "$dir/ctl.sock"
data_dir = "$dir/data"
relay = "disabled"
$share_toml
EOF
    # optional extra TOML for this node (e.g. a [reputation] block)
    [ -n "${EXTRA_CONFIG:-}" ] && printf '%s\n' "$EXTRA_CONFIG" >> "$dir/config.toml"
    "$BIN/filestrd" --config "$dir/config.toml" -vv 2> "$dir/daemon.log" &
    PIDS+=($!)
    declare -g "PID_${name}=$!"   # so tests can SIGHUP a specific node
    local i
    for i in $(seq 1 100); do
        if "$BIN/filestrctl" --socket "$dir/ctl.sock" status > /dev/null 2>&1; then
            return 0
        fi
        sleep 0.1
    done
    die "daemon $name did not come up"
}

# restart_node <name> — kill and relaunch a node reusing its data/state dirs
restart_node() {
    local name="$1"
    local dir="$TESTDIR/$name"
    local pidvar="PID_${name}"
    kill "${!pidvar}" 2>/dev/null || true
    sleep 0.5
    "$BIN/filestrd" --config "$dir/config.toml" -vv 2>> "$dir/daemon.log" &
    PIDS+=($!)
    declare -g "PID_${name}=$!"
    local i
    for i in $(seq 1 100); do
        if "$BIN/filestrctl" --socket "$dir/ctl.sock" status > /dev/null 2>&1; then
            return 0
        fi
        sleep 0.1
    done
    die "daemon $name did not restart"
}

fctl() {
    local name="$1"
    shift
    "$BIN/filestrctl" --socket "$TESTDIR/$name/ctl.sock" "$@"
}

node_id() {
    fctl "$1" --json status | jq -r .endpoint_id
}
