#!/bin/bash
set -u
B=/mnt/ssd/caches/hermes-target/release/hilo
R=/home/kara/.hilo/worktrees/heading
M=/tmp/dftip-1123/wmnt3
F=apps/api/src/config.ts
cd /tmp/dftip-1123 || exit 1
printf 'backing remote: '; git -C "$R" remote get-url origin
setfattr -n user.vfs.role -v backing-control "$R/$F"; echo "set backing rc=$?"
getfattr -n user.vfs.role "$R/$F"; echo "get backing rc=$?"
"$B" workspace mount "$M" --manifest ws7.yaml --daemon; echo "mount rc=$?"
sleep 3
getfattr -n user.vfs.role "$M/heading/$F"; echo "get workspace rc=$?"
"$B" meta --set user.vfs.role --value write-control "$M/heading/$F"; echo "write workspace rc=$?"
fusermount3 -u "$M"; echo "unmount rc=$?"
