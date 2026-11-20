#!/bin/bash
# walkbook-verify-TIP.sh — arms the 2026-11-20 fire left UNVERIFIED, corrected.
# v1 probe bugs fixed here: (a) repo-relative fixture paths (monorepo: apps/...),
# (b) NO pipes around exit-code checks, (c) mount window excludes the sleep,
# (d) DF-70 tested on its ACTUAL fix surface: hilo WORKSPACE mount (f9efdd7),
# plus the single-mount variant for completeness.
# REQUIRES: a TIP binary (>= 08267f3). If /home/kara/warpfs-wt-df49/target/release/hilo
# exists, use it; else build first: cd <worktree> && cargo build -p hilo-cli --release.
HILO=${HILO:-/home/kara/warpfs-wt-df49/target/release/hilo}
[ -x "$HILO" ] || { echo "no tip binary at $HILO — build first"; exit 1; }
"$HILO" --version
REPO=${REPO:?set REPO to a real foreign clone}
REPO2=${REPO2:-/tmp/df49/second}   # second clone for the workspace manifest
MNT=/tmp/df49/mnt
WMT=/tmp/df49/wmnt
SRC=apps/api/src/config.ts         # adjust to the clone's layout
LOG=/tmp/df49/tip-run.log
mkdir -p "$MNT" "$WMT"
exec > "$LOG" 2>&1
set +e
cd "$REPO" || exit 1

echo "=== T1 REV-WARPFS-9 AC: serve --http --port on the tip ==="
"$HILO" serve --help 2>&1 | grep -c -- '--port'   # expect >= 1 (help documents it)
"$HILO" serve --http --port 8767 > /tmp/df49/t1.log 2>&1 &
SRV=$!
sleep 4
ps -p $SRV >/dev/null && echo "serve ALIVE on 8767" || { echo "serve DIED:"; tail -2 /tmp/df49/t1.log; }
curl -s -o /dev/null -w "health=%{http_code} " http://127.0.0.1:8767/health
curl -s -o /dev/null -w "metrics=%{http_code} " http://127.0.0.1:8767/metrics
curl -s -o /dev/null -w "unknown=%{http_code}\n" http://127.0.0.1:8767/nope
kill $SRV 2>/dev/null; wait $SRV 2>/dev/null

echo "=== T2 DF-WARPFS-70 AC on its fix surface: WORKSPACE mount ==="
# pre-mount meta on repo1's file
"$HILO" meta --set user.vfs.role --value library "$SRC"; echo "meta-set rc=$?"
# two-repo workspace manifest
cat > /tmp/df49/workspace.yaml <<EOF
repos:
  - name: heading
    path: $REPO
  - name: second
    path: $REPO2
EOF
fusermount3 -u "$WMT" >/dev/null 2>&1; sleep 1
"$HILO" workspace mount "$WMT" --manifest /tmp/df49/workspace.yaml 2>&1 | head -3
echo "workspace-mount rc=$?"
sleep 3
# AC: xattr visible through the WORKSPACE mount + write-through no longer EROFS
getfattr -n user.vfs.role "$WMT/heading/$SRC" 2>&1; echo "getfattr-wsmount rc=$?"
"$HILO" meta --set user.vfs.role --value through-wmount "$WMT/heading/$SRC" 2>&1 | head -2
echo "meta-write-wsmount rc=$? (PASS = rc 0 and value reads back)"
fusermount3 -u "$WMT" >/dev/null 2>&1

echo "=== T3 single-mount DF-70 halves for comparison ==="
"$HILO" mount "$MNT" --daemon >/dev/null 2>&1; sleep 2
getfattr -n user.vfs.role "$MNT/$SRC" 2>&1 | head -2
"$HILO" meta --set user.vfs.role --value x "$MNT/$SRC" 2>&1 | head -1; echo "single-mount-write rc=$?"
DP=$(pgrep -f "hilo mount $MNT" | head -1); [ -n "$DP" ] && kill "$DP" && sleep 2

echo "=== T4 DF-WARPFS-156/176 AC: kill daemon -> log durable + self-heal ==="
"$HILO" mount "$MNT" --daemon 2>&1 | head -1   # banner names the log
sleep 2
DP=$(pgrep -f "hilo mount $MNT" | head -1); echo "daemon=$DP"; kill "$DP"; sleep 2
ls -la /home/kara/.vfs/logs/mount-daemon.log; tail -2 /home/kara/.vfs/logs/mount-daemon.log
timeout 3 ls "$MNT" >/dev/null 2>&1; echo "ls-mnt rc=$? (tip self-heal expected: 0 clean or explicit dead-mount message)"
grep -c "df49/mnt" /proc/mounts
fusermount3 -u "$MNT" >/dev/null 2>&1

echo "=== T5 PERF-011 AC: cold-mount + first stats, clean window ==="
for i in 1 2 3 4 5; do
  S=$(date +%s.%N); "$HILO" mount "$MNT" --daemon >/dev/null 2>&1; T=$(date +%s.%N)
  "$HILO" graph stats >/dev/null 2>&1; E=$(date +%s.%N)
  echo "arm$i mount=$(echo "$T-$S"|bc)s first-stats=$(echo "$E-$T"|bc)s"
  DP=$(pgrep -f "hilo mount $MNT" | head -1); [ -n "$DP" ] && kill "$DP" 2>/dev/null
  fusermount3 -u "$MNT" >/dev/null 2>&1; sleep 1
done
echo "=== TIP-DONE (also re-run hyperfine stats/impact medians; label host load) ==="
uptime
