#!/usr/bin/env bash
# Run the remaining bash tests (only the Grayjay tsc/build guard now). The daemon
# e2e suite is Rust (`cargo test -p filestrd --features grayjay`) and the Grayjay
# plugin-runtime suite is node (`cd grayjay-plugin && npm test`). Each test is
# hermetic: its own tmpdir, isolated daemons on localhost, relay disabled.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

(cd ../.. && cargo build --workspace) || exit 1

fail=0
for t in test-*.sh; do
    echo "=== $t"
    if bash "$t"; then
        echo "--- PASS $t"
    else
        echo "--- FAIL $t"
        fail=1
    fi
done
exit "$fail"
