#!/usr/bin/env bash
# DF-WARPFS-33 AC1: 8 concurrent `hilo graph stats` + `hilo graph impact`
# invocations on a warmed corpus must ALL exit 0 (no 'Conflicting lock').
# Shape mirrors the dogfood probe that measured 7/8 failures pre-fix.
set -u
CORPUS="$1"
HILO="$2"
[ -d "$CORPUS" ] || { echo "usage: $0 <corpus-dir> <hilo-binary>" >&2; exit 2; }
[ -x "$HILO" ] || { echo "hilo binary not executable: $HILO" >&2; exit 2; }

cd "$CORPUS" || exit 2
TARGET_FILE="$(find . -name '*.rs' -o -name '*.py' -o -name '*.ts' -o -name '*.go' 2>/dev/null | sed 's|^\./||' | sort | head -1)"
[ -n "$TARGET_FILE" ] || { echo "no indexable source file found in corpus" >&2; exit 2; }
echo "corpus=$CORPUS target=$TARGET_FILE"

pids=()
for i in 1 2 3 4; do
  ( "$HILO" graph stats >"/tmp/df33_stats_$i.log" 2>&1; echo "$?" >"/tmp/df33_stats_$i.rc" ) &
  ( "$HILO" graph impact "$TARGET_FILE" >"/tmp/df33_impact_$i.log" 2>&1; echo "$?" >"/tmp/df33_impact_$i.rc" ) &
done
wait

fails=0
for i in 1 2 3 4; do
  for kind in stats impact; do
    rc="$(cat "/tmp/df33_${kind}_$i.rc")"
    echo "  ${kind}[$i] rc=$rc"
    [ "$rc" = "0" ] || fails=$((fails+1))
    grep -q "Conflicting lock" "/tmp/df33_${kind}_$i.log" && { echo "  LOCK CONFLICT in ${kind}[$i]"; fails=$((fails+1)); }
  done
done
echo "FANOUT_RESULT fails=$fails of 8"
[ "$fails" = "0" ]