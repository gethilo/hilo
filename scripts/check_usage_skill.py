#!/usr/bin/env python3
"""
Guard: skills/hilo-usage/SKILL.md must match the shipped surface.

Checks three drift classes:
1. Tool count: skill must not claim "17 tools" when surface has 18
2. Gitignore: skill must not claim "hilo init writes no .gitignore entries"
   when init.rs actually installs managed ignore entries
3. Go file-form impact: skill must not claim "file-form impact is empty on Go"
   when the resolver actually handles Go files

Exit codes:
  0  all claims match shipped surface
  1  stale claim detected (guard fails)
  2  infrastructure error (missing file, parse failure)
"""

import re
import sys
from pathlib import Path

SKILL_PATH = Path("skills/hilo-usage/SKILL.md")
INIT_RS_PATH = Path("hilo-cli/src/commands/init.rs")
TOOLS_MOD_RS_PATH = Path("hilo-mcp/src/tools/mod.rs")


def count_shipped_tools() -> int:
    """Count vfs_* tool definitions in hilo-mcp/src/tools/mod.rs."""
    if not TOOLS_MOD_RS_PATH.exists():
        return -1
    content = TOOLS_MOD_RS_PATH.read_text()
    # Match: name: "vfs_<name>".into(),
    matches = re.findall(r'name:\s*"vfs_[^"]+"\s*\.into\(\)', content)
    return len(matches)


def init_installs_gitignore() -> bool:
    """Check if hilo-cli/src/commands/init.rs has managed ignore constants."""
    if not INIT_RS_PATH.exists():
        return False
    content = INIT_RS_PATH.read_text()
    # Look for the managed ignore header constant
    return "HILO_MANAGED_IGNORE_HEADER" in content and "HILO_DUCKDB_IGNORE_ENTRY" in content


def check_skill_for_stale_claims(skill_content: str) -> list[str]:
    """Return list of stale claims found in the skill file."""
    stale = []
    
    # Check 1: Tool count
    # Look for "17 tools" or "17 MCP tools" claims
    if re.search(r'\b17\s+(?:MCP\s+)?tools\b', skill_content, re.IGNORECASE):
        shipped = count_shipped_tools()
        if shipped > 0 and shipped != 17:
            stale.append(
                f"Tool count mismatch: skill claims 17 tools but surface has {shipped}. "
                f"Update '17 tools' → '{shipped} tools' in the skill."
            )
    
    # Check 2: Gitignore claim
    # Look for "writes no .gitignore entries" or "no .gitignore" claims
    if re.search(r'writes\s+no\s+\.gitignore\s+entries', skill_content, re.IGNORECASE):
        if init_installs_gitignore():
            stale.append(
                "Gitignore claim stale: skill says 'hilo init writes no .gitignore entries' "
                "but init.rs installs managed ignore entries (HILO_MANAGED_IGNORE_HEADER). "
                "Remove or update this claim."
            )
    
    # Check 3: Go file-form impact
    # Look for "file-form impact is empty on Go" or similar
    if re.search(r'file-form\s+impact\s+is\s+empty\s+on\s+Go', skill_content, re.IGNORECASE):
        stale.append(
            "Go file-form claim stale: skill says 'file-form impact is empty on Go' "
            "but the resolver handles Go files (resolves_nested_go_file_to_module_package test passes). "
            "Update to reflect that file-form works for Go via package resolution."
        )
    
    return stale


def main():
    if not SKILL_PATH.exists():
        print(f"ERROR: skill file not found: {SKILL_PATH}", file=sys.stderr)
        return 2
    
    skill_content = SKILL_PATH.read_text()
    stale_claims = check_skill_for_stale_claims(skill_content)
    
    if stale_claims:
        print("FAIL: skills/hilo-usage/SKILL.md has stale claims:", file=sys.stderr)
        for i, claim in enumerate(stale_claims, 1):
            print(f"  {i}. {claim}", file=sys.stderr)
        return 1
    
    print("PASS: skills/hilo-usage/SKILL.md matches shipped surface")
    return 0


if __name__ == "__main__":
    sys.exit(main())
