# AGENTS

Project guidance for coding agents and contributors working in this repository.

This repo currently uses this file as the canonical contributor/agent policy. If dedicated files like `copilot-instructions.md`, `CLAUDE.md`, or `.cursorrules` are added later, keep their guidance consistent with this document.

## Scope and priorities

- Keep changes focused and minimal.
- Fix root causes rather than layering temporary workarounds.
- Avoid unrelated refactors while implementing a requested change.

## Cross-agent operating rules (Copilot/Claude/Cursor aligned)

- Be concise and direct in code and communication.
- Prefer the smallest change that fully solves the requested problem.
- Complete work end-to-end when feasible (implement + validate), not just analysis.
- If requirements are ambiguous, choose the simplest interpretation that matches existing behavior; ask only when ambiguity changes outcomes materially.
- Do not add speculative features, broad rewrites, or unrelated cleanup.
- Do not commit, create branches, or alter repository history unless explicitly requested.
- Keep security/safety in mind: avoid introducing secret leakage paths or unsafe defaults.

## Rust code style

- Follow existing style and naming patterns in the repository.
- Keep functions small and explicit; prefer straightforward control flow.
- Avoid one-letter variable names except for tight loop indices.
- Do not add new dependencies unless they materially simplify or harden the implementation.
- Preserve public API behavior unless the task explicitly requires a change.

## Commenting and documentation rules

Comments should be useful, not verbose.

- Prefer concise doc comments on public types/functions and non-obvious behavior.
- Explain why or constraints, not line-by-line what the code already says.
- Remove stale, duplicated, or obvious comments during touched-file edits.
- Do not add decorative comments or banner noise.
- Keep markdown docs in `docs/` for technical detail; keep root `README.md` as product/operator overview.

## Testing policy (TDD-esque)

Use a test-first mindset for behavior changes:

1. Add or adjust a test that captures expected behavior (or reproduces bug).
2. Implement the code change.
3. Re-run targeted tests first, then broader suites.

Practical expectations:

- For new features or bug fixes, include test coverage where there is an established place to do so.
- Prefer narrowly scoped tests before running full test suite.
- Do not rewrite large test areas unless required by the change.

## Validation checklist

Before finishing:

- Run relevant tests (`cargo test <filter>`), then `cargo test` when practical.
- Run clippy with strict settings (`cargo clippy --all-targets --all-features -- -D warnings`) and fix all findings.
- Ensure the repository remains fully clean under `#![deny(warnings)]` and clippy deny settings, including pre-existing warnings/lints.
- Confirm docs updated when behavior or interfaces changed.

## MCP-specific notes

- Preserve initialization gating semantics for MCP methods.
- Keep tool results structurally stable unless intentional contract changes are requested.
