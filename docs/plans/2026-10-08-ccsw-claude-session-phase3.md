# Provider session profiles

Goal: implement phase 3 of the provider design and release v0.6.0.

Architecture: retain the common session command and sharing code. Keep profile credential ownership in `claude::session`. Directory mappings resolve independently by provider. Preserve the existing POSIX exec and Windows child-status contract.

Tech stack: Rust, serde, the existing private file and Keychain helpers, filesystem-only test fixtures and fake child executables.

Spec: `docs/specs/2026-10-07-ccsw-claude-provider-design.md`, sections 4–6 and 10.

## Global constraints

- Work in the current checkout; do not create a worktree.
- Use temporary homes, fake Keychain responses and local HTTP fixtures in tests.
- Do not read or change real credentials or call live authentication endpoints.
- Keep the existing provider's session behavior and all existing regression tests.
- POSIX exec cannot run an exit callback. Reconcile profile tokens before later usage, refresh, switching and launch; Windows also reconciles after exit.
- Inspect only managed profile PID records for credential safety. Do not add system-wide process discovery or a running-instance display.
- Treat unreadable credentials or session records as uncertain ownership. Do not overwrite or refresh them.
- No first-launch migration, transfer commands, MCP mirroring, or automatic updates.

## Review focus

1. Default and session Keychain service separation, including Unicode and raw environment paths.
2. A running session, duplicate token identities, identity drift, unreadable metadata and token rotations.
3. Mixed-provider mappings, legacy mapping files, removal and reused slot numbers.
4. Environment overrides, same-account fast paths, nested sessions and missing executables.
5. Sharing manifest boundaries, private files, Windows behavior and existing command compatibility.

## Task 1: Provider mappings

Files: `src/store/mappings.rs`, mapping consumers in `src/session.rs` and `src/switcher.rs`, their tests.

Interface: mapping operations take a provider; exact removal optionally takes one. `entries` returns path, provider and identity. Persist schema 2 as path → entry array, with a provider on each entry. Read schema 1 single entries as the original provider.

- Add tests for two mappings at one directory, independent nearest ancestors, provider-specific prune/remove and a legacy round trip.
- Run `env -u CODEX_HOME cargo test store::mappings`. Expected: new behavior fails before implementation.
- Implement the schema and update consumers without changing their provider behavior yet.
- Run the mapping tests and full suite. Expected: all pass.
- Commit `feat(session): scope directory mappings by provider`.

## Task 2: Profile credentials and ownership

Files: new `src/claude/session.rs`, `src/claude/mod.rs`, `src/claude/live.rs`, `src/paths.rs`, `src/collect.rs`, `src/switcher.rs` and tests.

Interface: read a profile with an injected SecurityCli; reconcile only matching identity and newer tokens under the store lock; inspect managed profile activity conservatively. Refresh guards include session-owned slots and fingerprints. Profile bootstrap never overwrites a live or unreadable profile. A launch marker covers the exec startup window.

- Write failing tests for file and hashed-Keychain rotation, identity drift, active/unreadable profiles and duplicate credential refresh protection.
- Implement reads, reconciliation, profile bootstrap and guards; preserve local account-independent config keys.
- Exercise local HTTP fixtures to prove no refresh for owned credentials, and fresh profile tokens are used for usage.
- Run focused tests then the full suite. Expected: all pass.
- Commit `feat(session): manage isolated profile credentials safely`.

## Task 3: Commands and shared files

Files: `src/session.rs`, `src/cli/session.rs`, `tests/session_run.rs`, new session CLI tests, help and completion sources where applicable.

Interface: `run` and `env` accept account or provider; default targets carry a provider. Infer a unique mapped provider, otherwise require a selector when the roster is mixed. `unmap DIR [provider]` removes one or both mappings. Pin the provider's home and scrub only its authentication overrides. Check the original provider's credential-store setting only when touching that provider.

- Add failing CLI tests using fake executables for profile seeding, argument forwarding, provider selection, environment exports/unsets, missing binaries and mapping resolution.
- Dispatch preparation to the new provider profile implementation; reuse the manifest sharing engine with provider-specific item lists.
- Preserve plain same-account launch when no home is preset. Add `--require-session` to refuse this fast path when isolation is required.
- Verify private files, no-share/pruning, history sharing and Windows history refusal.
- Run focused tests and the full suite. Expected: all pass.
- Commit `feat(session): launch and map provider profiles`.

## Task 4: Documentation, review and release

Files: README, handover, changelog, spec, package version and lockfile.

- Document commands, ownership limits, token reconciliation and release v0.6.0.
- Run fmt, clippy with warnings denied, full tests, locked build and isolated CLI smoke tests.
- Obtain one independent whole-branch review. Fix important findings with a failing regression test, then rerun the suite.
- Create a normal PR, wait for all CI, squash merge the verified head, tag and publish the merged commit.
- Verify release artifacts and checksums. Remove only this task's temporary files.

## Implementation validation

Tasks 1–3 are complete. One independent whole-branch review found five important
ownership issues. Regression tests first reproduced each issue; one fix pass added
atomic launch reservations, fingerprint consume locks, mutation preflight, stale
profile invalidation, and managed Keychain cleanup. Duplicate slots share one
mutation lock. No minor findings were deferred.

The final local gates pass: 484 tests, formatting, Clippy with warnings denied,
a locked build, and eight isolated CLI smoke checks. Tests use temporary homes,
fake CLI and Keychain implementations, and local HTTP servers.

Decisions: preserve POSIX exec and reconcile on later commands; conservatively
inspect managed PID records; use the provided checkout and temporary ledger;
clear both provider homes for a bare `env --unset`.
