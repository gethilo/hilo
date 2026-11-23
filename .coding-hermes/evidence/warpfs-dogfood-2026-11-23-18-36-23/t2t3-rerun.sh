#!/bin/bash
# Clean T2/T3 re-run: pipe-free rc capture, schema-correct workspace manifest.
# Tick: warpfs-dogfood-2026-11-23-18-36-23
HILO=/mnt/ssd/caches/hermes-target/release/hilo
REPO=/tmp/dftip-1123/heading
SRC=apps/api/src/config.ts
WMT=/tmp/dftip-1123/wmnt2
MNT=/tmp/dftip-1123/mnt2
LOG=/tmp/dftip-1123/t2t3-rerun.log
mkdir -p "$WMT" "$MNT"
exec > "$LOG" 2>&1
set +e
cd "$REPO" || exit 1

echo "=== P0: where does the pre-mount xattr actually live? ==="
df -T /tmp/dftip-1123 | tail -1
getfattr -n user.vfs.role "$REPO/$SRC" 2>&1; echo "underlying-getfattr rc=$?"
wc -l "$REPO/.vfs/annotations.jsonl" 2>&1
tail -2 "$REPO/.vfs/annotations.jsonl" 2>&1

echo "=== T2-fix: workspace mount with schema-correct manifest ==="
cat > /tmp/dftip-1123/workspace2.yaml <<EOF
repos:
  - name: heading
    at: $REPO
  - name: second
    at: /tmp/dftip-1123/second
EOF
fusermount3 -u "$WMT" >/dev/null 2>&1; sleep 1
"$HILO" workspace mount "$WMT" --manifest /tmp/dftip-1123/workspace2.yaml > /tmp/dftip-1123/wsm.out 2> /tmp/dftip-1123/wsm.err
echo "workspace-mount rc=$? (clean, no pipe)"
sleep 3
mount | grep -c dftip-1123/wmnt2
getfattr -n user.vfs.role "$WMT/heading/$SRC" > /tmp/dftip-1123/wsm-getfattr.out 2>&1
echo "wsmount-getfattr rc=$?"
cat /tmp/dftip-1123/wsm-getfattr.out
"$HILO" meta --set user.vfs.role --value through-wmount "$WMT/heading/$SRC" > /tmp/dftip-1123/wsm-set.out 2>&1
echo "wsmount-meta-set rc=$?"
cat /tmp/dftip-1123/wsm-set.out
getfattr -n user.vfs.role "$WMT/heading/$SRC" 2>&1
echo "wsmount-readback rc=$?"
fusermount3 -u "$WMT" >/dev/null 2>&1

echo "=== T3-fix: single mount, clean rc ==="
fusermount3 -u "$MNT" >/dev/null 2>&1; sleep 1
"$HILO" mount "$MNT" --daemon > /tmp/dftip-1123/mnt2-mount.out 2>&1
echo "mount rc=$?"
sleep 3
getfattr -n user.vfs.role "$MNT/$SRC" > /tmp/dftip-1123/mnt-getfattr.out 2>&1
echo "singlemount-getfattr rc=$?"
cat /tmp/dftip-1123/mnt-getfattr.out
"$HILO" meta --set user.vfs.role --value x "$MNT/$SRC" > /tmp/dftip-1123/mnt-set.out 2>&1
echo "singlemount-meta-set rc=$?"
cat /tmp/dftip-1123/mnt-set.out
echo "=== T3b: who is the daemon? (pgrep -x vs -f) ==="
pgrep -ax hilo
pgrep -af "hilo mount" || echo "pgrep -f 'hilo mount': NO MATCH"
echo "=== T3c: held fds of the daemon ==="
DP=$(pgrep -x hilo | head -1)
[ -n "$DP" ] && ls -l /proc/$DP/fd 2>/dev/null | head -12 && echo "daemon pid=$DP"
fusermount3 -u "$MNT" >/dev/null 2>&1
echo "=== DONE ==="
