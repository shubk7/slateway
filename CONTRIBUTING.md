# Contributing

Setup: `docs/DEVELOPMENT.md`. Build, test and lint commands: `AGENTS.md`. Picking work: open tasks in `docs/ROADMAP.md`.

- **Flow:** issue, then branch, then small PR. One concern per PR.
- **Commits:** Conventional Commits (`feat:`, `fix:`, `docs:`, `refactor:`, `test:`), signed off with `git commit -s` (DCO).
- **RFCs:** protocol, security and architecture changes start as a PR to the relevant doc, labelled `rfc`, before any code. Accepted decisions are logged in `docs/DECISIONS.md`.
- **Style:** `rustfmt` and `clippy -D warnings` for Rust, the project lint rules for Kotlin, no warnings in CI.
- **Platform changes:** say which OS and device you tested on. Untested platform code must be labelled as such.
- **AI-assisted contributions** are welcome. You are responsible for understanding, testing and reviewing every line you submit.
- **Privacy:** contributions adding telemetry, analytics, ads or accounts are rejected.
