#!/bin/bash
# Workspace mount arms: workspace-schema (name+url local path) and project-schema (repos[].at).
HILO=/mnt/ssd/caches/hermes-target/release/hilo
REPO=/tmp/dftip-1123/heading
SRC=apps/api/src/config.ts
WMT=/tmp/dftip-1123/wmnt3
LOG=/tmp/dftip-1123/ws-arms.log
mkdir -p "$WMT"
exec > "$LOG" 2>&1
set +e
cd "$REPO" || exit 1

echo "=== W1: workspace schema, repos with url=<local clone path> ==="
cat > /tmp/dftip-1123/ws3.yaml <<EOF
repos:
  - name: heading
    url: /tmp/dftip-1123/heading
  - name: second
    url: /tmp/dftip-1123/second
EOF
fusermount3 -u "$WMT" >/dev/null 2>&1; sleep 1
"$HILO" workspace mount "$WMT" --manifest /tmp/dftip-1123/ws3.yaml > /tmp/dftip-1123/wsm3.out 2> /tmp/dftip-1123/wsm3.err
echo "ws-mount rc=$?"
cat /tmp/dftip-1123/wsm3.out /tmp/dftip-1123/wsm3.err
sleep 3
ls "$WMT" 2>&1 | head -4
getfattr -n user.vfs.role "$WMT/heading/$SRC" 2>&1; echo "ws-getfattr rc=$?"
"$HILO" meta --set user.vfs.role --value through-ws "$WMT/heading/$SRC" > /tmp/dftip-1123/ws-set.out 2>&1
echo "ws-meta-set rc=$?"; cat /tmp/dftip-1123/ws-set.out
getfattr -n user.vfs.role "$WMT/heading/$SRC" 2>&1
fusermount3 -u "$WMT" >/dev/null 2>&1

echo "=== W2: project schema (init-written manifest + second repo appended) ==="
cp "$REPO/.vfs/manifest.yaml" /tmp/dftip-1123/proj-manifest.yaml
cat >> /tmp/dftip-1123/proj-manifest.yaml <<EOF
repos-test:
  - name: second
    at: /tmp/dftip-1123/second
EOF
fusermount3 -u "$WMT" >/dev/null 2>&1; sleep 1
"$HILO" workspace mount "$WMT" --manifest /tmp/dftip-1123/proj-manifest.yaml > /tmp/dftip-1123/wsp.out 2> /tmp/dftip-1123/wsp.err
echo "proj-mount rc=$?"
cat /tmp/dftip-1123/wsp.out /tmp/dftip-1123/wsp.err | head -6
sleep 3
ls "$WMT" 2>&1 | head -4
fusermount3 -u "$WMT" >/dev/null 2>&1
echo "=== WS-ARMS DONE ==="
