# Rapina (agents)

Rust web framework workspace: `rapina` (core), `rapina-macros`, `rapina-cli`. Docs site under `docs/` (Zola).

## Verify

```bash
just verify
```

That is the local gate for the CI **Check** job (version pins, fmt, clippy `--all-features`, build, tests). Prefer it over one-off cargo invocations. `just verify-quick` is default-features only for a faster inner loop; do not treat it as CI-green. Extra CI jobs (feature matrix, MSRV, deny, examples, doc tests) stay in GitHub Actions unless you need them for the change.

The justfile puts rustup’s `~/.cargo/bin` ahead of other `cargo` installs so `rust-toolchain.toml` (`nightly-2026-02-15`) applies.

## Project judgment

- Keep the public API boring: annotate handlers, `.discover()`, ship. Do not add router ceremony without a strong reason.
- Prefer making illegal states unrepresentable in types over new prose rules.
- Release tags and crates.io publishes are maintainer-only. Do not push `v*` tags or run `cargo publish`.
