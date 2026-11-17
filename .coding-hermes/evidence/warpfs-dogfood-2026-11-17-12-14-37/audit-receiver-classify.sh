#!/usr/bin/env bash
# Classify graph audit bucket entries as exported func / method on exported type /
# method on UNexported type / python def (no Go receiver concept).
# Usage: audit-receiver-classify.sh <audit.json> <corpus-root>
set -u
J="$1"; ROOT="$2"
jq -r '.buckets | to_entries[]
       | .key as $b
       | .value.symbols[]
       | [$b, .name, .file, (.line|tostring)] | @tsv' "$J" > /tmp/audit-syms.tsv
printf 'bucket\tsymbol\tfile\tline\tkind\treceiver\n'
while IFS=$'\t' read -r b name file line; do
  src="$ROOT/$file"
  if [ ! -f "$src" ]; then
    printf '%s\t%s\t%s\t%s\tMISSING_FILE\t-\n' "$b" "$name" "$file" "$line"; continue
  fi
  def=$(sed -n "${line}p" "$src")
  case "$def" in
    "def "*|"async def "*)
      printf '%s\t%s\t%s\t%s\tpy-def\t-\n' "$b" "$name" "$file" "$line"
      ;;
    "func ("*)
      # receiver text is everything between the first "(" and the next ")"
      recv=$(printf '%s' "$def" | sed -E 's/^func \(([^)]*)\).*/\1/')
      # receiver type is the LAST whitespace-separated token, minus any pointer star
      rtype=$(printf '%s' "$recv" | awk '{print $NF}' | tr -d '*')
      first=$(printf '%s' "$rtype" | cut -c1)
      case "$first" in
        [A-Z]) kind="method-exported-type" ;;
        [a-z]) kind="method-unexported-type" ;;
        *) kind="method-other" ;;
      esac
      printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$b" "$name" "$file" "$line" "$kind" "$rtype"
      ;;
    "func "*)
      fname=$(printf '%s' "$def" | sed -E 's/^func ([A-Za-z0-9_]+).*/\1/')
      first=$(printf '%s' "$fname" | cut -c1)
      case "$first" in
        [A-Z]) kind="func-exported" ;;
        [a-z]) kind="func-unexported" ;;
        *) kind="func-other" ;;
      esac
      printf '%s\t%s\t%s\t%s\t%s\t-\n' "$b" "$name" "$file" "$line" "$kind"
      ;;
    *) printf '%s\t%s\t%s\t%s\tUNMATCHED_DEF\t-\n' "$b" "$name" "$file" "$line" ;;
  esac
done < /tmp/audit-syms.tsv
