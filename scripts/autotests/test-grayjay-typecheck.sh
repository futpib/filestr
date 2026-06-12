#!/usr/bin/env bash
# The Grayjay plugin is authored in TypeScript (grayjay-plugin/src/FilestrScript.ts)
# and type-checked against @types/grayjay-source, so a misnamed/mistyped source
# method is a COMPILE ERROR rather than a method Grayjay silently never calls
# (the getPlaylistsUser-vs-getUserPlaylists class of bug). This guards two things:
#   1. the TypeScript type-checks cleanly;
#   2. the committed FilestrScript.js (which filestrd embeds via include_str!) is
#      exactly what compiling the .ts produces — no hand-edits, no drift.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

command -v node > /dev/null 2>&1 || { echo "SKIP (node not installed)"; exit 0; }
command -v npm  > /dev/null 2>&1 || { echo "SKIP (npm not installed)";  exit 0; }

PLUGIN="$ROOT/grayjay-plugin"
cd "$PLUGIN"

# Need the toolchain + types. Use the lockfile when deps aren't present; if that
# can't be fetched (offline CI), skip rather than fail — the guard only applies
# where the toolchain is available.
if [ ! -d node_modules ]; then
    npm ci > "$TESTDIR/npm-ci.log" 2>&1 || { echo "SKIP (npm ci failed; see $TESTDIR/npm-ci.log)"; exit 0; }
fi

# 1. Type-check (no emit). A bad method name / signature fails here.
npx tsc -p tsconfig.json --noEmit || die "TypeScript type-check failed"

# 2. Drift check: compile to a temp file and compare with the committed output.
npx tsc -p tsconfig.json --outFile "$TESTDIR/built.js" || die "TypeScript compile failed"
if ! diff -u "$PLUGIN/FilestrScript.js" "$TESTDIR/built.js" > "$TESTDIR/plugin.diff"; then
    echo "--- committed FilestrScript.js differs from compiling FilestrScript.ts:" >&2
    head -n 40 "$TESTDIR/plugin.diff" >&2
    die "FilestrScript.js is stale — run 'npm --prefix grayjay-plugin run build' and commit"
fi

# 3. Sanity: the exact bug class is actually caught (a wrong method name must NOT
# compile). Proves the type-check has teeth, not just that valid code passes.
# A throwaway tsconfig extends the real one (same lib/strict/ambient types) but
# compiles a one-line probe so only the bogus-method error can fire.
printf 'source.getPlaylistsUser = function () { return []; };\n' > "$TESTDIR/probe.ts"
cat > "$TESTDIR/tsconfig.probe.json" <<JSON
{
  "extends": "$PLUGIN/tsconfig.json",
  "compilerOptions": { "noEmit": true, "outFile": null },
  "include": [
    "$TESTDIR/probe.ts",
    "$PLUGIN/node_modules/@types/grayjay-source/src/plugin.d.ts",
    "$PLUGIN/src/env.d.ts"
  ]
}
JSON
if npx tsc -p "$TESTDIR/tsconfig.probe.json" > "$TESTDIR/probe.log" 2>&1; then
    die "type-check accepted a bogus source method (getPlaylistsUser) — the guard has no teeth"
fi
grep -q "getPlaylistsUser" "$TESTDIR/probe.log" || { cat "$TESTDIR/probe.log" >&2; die "probe failed for an unexpected reason"; }

echo OK
