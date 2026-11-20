#!/bin/bash
# Focused re-run: corrects walkbook v1 probe bugs. SSD binary (pre-fix e518262 tree).
HILO=/mnt/ssd/caches/hermes-target/release/hilo
REPO=/tmp/df49/heading
MNT=/tmp/df49/mnt
LOG=/tmp/df49/use2.log
exec > "$LOG" 2>&1
set +e
cd "$REPO" || exit 1
SRC=apps/api/src/config.ts

echo "=== R1 COV gate exit codes, no pipes ==="
"$HILO" graph surfaces > /tmp/df49/r1-surfaces.out 2> /tmp/df49/r1-surfaces.err
echo "surfaces rc=$?"; tail -1 /tmp/df49/r1-surfaces.err
"$HILO" graph coverage-links > /tmp/df49/r1-cov.out 2> /tmp/df49/r1-cov.err
echo "coverage-links rc=$?"; tail -1 /tmp/df49/r1-cov.err

echo "=== R2 DF-70 arm with a real file (pre-fix binary) ==="
"$HILO" meta --set user.vfs.role --value library "$SRC"; echo "meta-set rc=$?"
"$HILO" meta "$SRC" | head -3
fusermount3 -u "$MNT" >/dev/null 2>&1; sleep 1
"$HILO" mount "$MNT" --daemon; echo "mount rc=$?"
sleep 3
getfattr -n user.vfs.role "$MNT/$SRC" 2>&1; echo "getfattr-through-mount rc=$? (DF-70 premise live if attribute absent)"
echo "--- meta WRITE through the mount (pre-fix): "
"$HILO" meta --set user.vfs.role --value through-mount "$MNT/$SRC" 2>&1 | head -2; echo "meta-through-mount rc=$?"
DP=$(pgrep -f "hilo mount $MNT" | head -1); [ -n "$DP" ] && kill "$DP" && sleep 2
echo "--- durable log (banner path) after daemon kill:"
ls -la /home/kara/.vfs/logs/mount-daemon.log 2>&1; tail -3 /home/kara/.vfs/logs/mount-daemon.log 2>&1
echo "--- stale mount state:"
timeout 3 ls "$MNT" >/dev/null 2>&1; echo "ls-mnt rc=$?"
grep -c "df49/mnt" /proc/mounts
fusermount3 -u "$MNT" >/dev/null 2>&1; echo "manual unmount rc=$?"

echo "=== R3 serve --http on 8766: whose /health? ==="
pgrep -af 'off-by-one' | head -1
"$HILO" serve --http > /tmp/df49/r3-serve.log 2>&1 &
SRV=$!
sleep 4
ps -p $SRV >/dev/null && echo "hilo-serve ALIVE pid=$SRV" || echo "hilo-serve EXITED (log:)"
tail -3 /tmp/df49/r3-serve.log
curl -s -i http://127.0.0.1:8766/health | head -6
echo "---"
curl -s -o /dev/null -w "metrics_code=%{http_code}\n" http://127.0.0.1:8766/metrics
kill $SRV 2>/dev/null

echo "=== R4 manifest port knob (pre-fix): interfaces.mcp.port ==="
grep -n 'mcp' .vfs/manifest.yaml | head -4

echo "=== R5 clean perf: mount window without sleep, impact on real path ==="
for i in 1 2 3; do
  S=$(date +%s.%N)
  "$HILO" mount "$MNT" --daemon >/dev/null 2>&1
  T=$(date +%s.%N)
  "$HILO" graph stats >/dev/null 2>&1
  E=$(date +%s.%N)
  echo "arm$i mount-start=$(echo "$T-$S" | bc)s first-stats=$(echo "$E-$T" | bc)s"
  DP=$(pgrep -f "hilo mount $MNT" | head -1); [ -n "$DP" ] && kill "$DP" 2>/dev/null
  fusermount3 -u "$MNT" >/dev/null 2>&1; sleep 1
done
hyperfine -N -w 2 -m 5 "$HILO graph impact $SRC --max-depth 2" 2>&1 | grep -E 'Time|±' | head -2
echo "=== DONE2 ==="
