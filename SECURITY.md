# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | ✅                 |

Hilo is in active development. Security patches are issued for the
latest release.

## Reporting a Vulnerability

**Do not open a public issue.** Send details to:

**wojonstech@gmail.com**

Include:
- Description of the vulnerability
- Steps to reproduce
- Affected version(s)
- Any proposed fix (if available)

You'll receive an acknowledgment within **48 hours**. We aim to publish
a fix within **7 days** of confirmation.

### Scope

- Hilo core daemon, CLI, and MCP server
- FUSE mount path traversal and permission bypass
- MCP server input validation
- Graph engine SQL injection
- Secrets leakage through xattrs or inventory files

### Out of Scope

- Denial of service through excessive graph queries
- Local privilege escalation when the attacker already has filesystem access
- Vulnerabilities in third-party dependencies (report upstream)
