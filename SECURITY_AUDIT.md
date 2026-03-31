# Security Audit — internet-radio-mcp

**Date:** 2026-03-31
**Scope:** Internet radio MCP server

---

## High Severity

### 1. URL Passed Directly to mpv

**File:** `src/operations/radio.rs:103-120`

User-supplied stream URLs are passed directly as arguments to `mpv`. While `Command::new()` doesn't invoke a shell, mpv interprets special URL schemes:
- `file:///etc/passwd` reads local files
- Protocol-specific exploits possible via crafted URLs

**Recommendation:** Parse URLs with the `url` crate and reject non-HTTP(S) schemes. Validate the host is not a private/loopback address.

---

### 2. pkill Kills All mpv Instances

**File:** `src/operations/radio.rs:127-130`

Stop uses `pkill mpv` which kills every mpv process on the system, not just the one this server started.

```rust
std::mem::forget(child);  // PID lost
// ... later ...
Command::new("pkill").arg("mpv")  // kills ALL mpv
```

**Recommendation:** Store the child PID and use `kill(pid, SIGTERM)` directly.

---

## Medium Severity

### 3. Custom URL Encoding

**File:** `src/operations/radio.rs:84-100`

Manual URL encoding function encodes spaces as `+` (form encoding, not standard URL encoding) and has hand-rolled hex encoding.

**Recommendation:** Use the `urlencoding` crate or `url::form_urlencoded`.

---

## Positive Findings

- No `unsafe` code
- Simple, small attack surface
