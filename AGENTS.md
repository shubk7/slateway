# AGENTS.md

Slateway is a free, GPLv3 platform that links Windows/Linux PCs with Android tablets (second screen, touch/pen, audio, mic, camera), USB-first. Solo maintainer, AI-assisted development. Goals and feature list: `docs/SCOPE.md`. Current milestone and tasks: `docs/ROADMAP.md`.

## Read before you touch

| Working on | Read first |
|---|---|
| Anything non-trivial | `docs/SCOPE.md`, `docs/ARCHITECTURE.md` |
| Wire format, messages, channels | `docs/PROTOCOL.md` |
| Pairing, crypto, consent, input injection, privacy | `docs/SECURITY.md` |
| OS or Android specific APIs, drivers, known gotchas | `docs/PLATFORMS.md` |
| Environment setup, dual-boot workflow | `docs/DEVELOPMENT.md` |
| Tests, latency measurement, reference devices | `docs/TESTING.md` |
| Picking a library or approach | `docs/DECISIONS.md` (check first, log new decisions) |

## Commands

Some do not exist until M0 creates them.

```
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo deny check
cd fuzz && cargo fuzz run <target>        # when parser code changes
cd android && ./gradlew assembleDebug testDebugUnitTest lintDebug
```

## Rules

1. Work on one `ROADMAP.md` task at a time. Do not add scope; propose it instead.
2. Platform-specific code lives only in `slateway-win` / `slateway-linux` behind the traits in `slateway-platform`. Core crates must build and test on both OSes with no platform code.
3. The maintainer runs one OS at a time. State which OS you verified on and mark code for the other as unverified in the commit message; CI is the cross-check.
4. Never guess API names, crate signatures or OS behaviour. Read the docs or crate source. If still unsure, write a time-boxed spike and say what is unverified.
5. Dependencies need a reason, a GPLv3-compatible license and a passing `cargo deny check`. No telemetry, analytics or network calls beyond the Slateway session.
6. Errors: `thiserror` in libraries, `anyhow` only in binaries. No `unwrap()`/`expect()` outside tests and startup invariants. Log with `tracing`; never log frame, input, audio or clipboard contents, or key material.
7. The latency rules in `ARCHITECTURE.md` are invariants, not suggestions.
8. Pending items in `DECISIONS.md` are not yours to decide: run the named spike or ask.

## Stop and ask the maintainer before finishing

- Any protocol change (also bump the version and update `PROTOCOL.md`).
- Anything in pairing, crypto or consent.
- Any `unsafe` block (needs a `// SAFETY:` comment) or Windows driver code.
- New dependencies, license changes, or new ADRs.
- Anything that touches the system outside the app: driver installs, kernel modules, udev rules, `bcdedit`, Secure Boot.

## Definition of done

CI green on Windows, Linux and Android; clippy and fmt clean; tests added (parser changes also update the fuzz target); the single-source doc for the area updated; `ROADMAP.md` checkbox ticked; ADR logged if a decision was made.
