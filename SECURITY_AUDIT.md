# Security Audit — internet-radio-mcp

**Date:** 2026-03-31
**Scope:** Internet radio MCP server

---

## Medium Severity

### 1. Custom URL Encoding (MEDIUM)

**File:** `src/operations/radio.rs:84-100`

Manual URL encoding function encodes spaces as `+` (form encoding, not standard URL encoding) and has hand-rolled hex encoding.

**Recommendation:** Use the `urlencoding` crate or `url::form_urlencoded`.

---

## Positive Findings

- No `unsafe` code
- Simple, small attack surface
- Stream URLs validated before passing to mpv
