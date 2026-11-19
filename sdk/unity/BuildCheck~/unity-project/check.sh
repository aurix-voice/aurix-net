#!/usr/bin/env bash
# Builds the package in a Unity editor with no manual steps: check.sh <path to the Unity editor binary> [web]
# One line per check on the output (AURIX <check> PASS|FAIL ...), the editor logs under Logs/, exit code 1 if any failed.
set -u
unity="${1:?usage: check.sh <path to the Unity editor binary> [web]}"
cd "$(dirname "$0")"
mkdir -p Logs
checks="Compile BuildPlayer"
[ "${2:-}" = web ] && checks="$checks BuildWeb"
# The editor upgrades a project of another version, or of none, and adds to its manifest the
# packages and built-in modules that came since: the project names the editor that opens it.
version=$("$unity" -version 2>/dev/null | grep -o -m 1 '[0-9]\+\.[0-9]\+\.[0-9]\+[a-z][0-9]\+')
[ -n "$version" ] || { echo "AURIX Version FAIL $unity -version printed no editor version"; exit 1; }
mkdir -p ProjectSettings
echo "m_EditorVersion: $version" > ProjectSettings/ProjectVersion.txt
# A lock file another editor left would keep its versions of what the package names.
rm -f Packages/packages-lock.json
cp Packages/manifest.json Logs/manifest.json
failed=0
for check in $checks; do
    "$unity" -batchmode -nographics -quit -projectPath . -executeMethod "AurixBuildCheck.$check" -logFile "Logs/$check.log"
    code=$?
    # first of all: a check that ran with packages the editor added says nothing about the package
    if ! cmp -s Packages/manifest.json Logs/manifest.json; then
        echo "AURIX Manifest FAIL the editor changed Packages/manifest.json, $check is not judged: the project held more than the package names"
        diff Logs/manifest.json Packages/manifest.json | grep -m 20 "^[<>]"
        cp Logs/manifest.json Packages/manifest.json
        exit 1
    fi
    grep -a "^AURIX " "Logs/$check.log" || echo "AURIX $check FAIL the editor left with code $code before the check ran: see Logs/$check.log"
    grep -a -m 20 "error CS\|Scripts have compiler errors" "Logs/$check.log"
    [ $code -eq 0 ] || failed=1
done
exit $failed
