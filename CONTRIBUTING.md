# Contributing to Rapina

Thanks for your interest in contributing to Rapina. This document outlines the process for contributing.

## Getting Started

1. Fork the repository
2. Clone your fork:
   ```bash
   git clone https://github.com/YOUR_USERNAME/rapina.git
   cd rapina
   ```
3. Add the upstream remote:
   ```bash
   git remote add upstream https://github.com/rapina-rs/rapina.git
   ```

## Development Setup

### Prerequisites

- Rust 1.85 or later
- Cargo

### Building

```bash
# Build all packages
cargo build

# Run tests
cargo test

# Run the CLI locally
cargo run -p rapina-cli -- new test-app
```

Preferred local gate (matches the CI Check job):

```bash
just verify
```

See `AGENTS.md` for what that covers. `just verify-quick` is a faster default-features loop while iterating.

### Project Structure

```
rapina/
├── rapina/          # Core framework
├── rapina-macros/   # Procedural macros
├── rapina-cli/      # CLI tool
└── docs/            # Documentation site (Zola)
```

## Claiming Issues

Want to work on something? Comment `/take` on any open issue and it will be assigned to you automatically. If you can't finish it, comment `/release` and we'll unassign you so someone else can pick it up.

## Making Changes

### Branch Naming

- `feat/description` - New features
- `fix/description` - Bug fixes
- `docs/description` - Documentation changes
- `refactor/description` - Code refactoring

### Commit Messages

Write clear, concise commit messages:

```
feat: add rate limiting middleware

- Implement token bucket algorithm
- Add configuration options
- Include tests
```

Use conventional commit prefixes:
- `feat:` - New feature
- `fix:` - Bug fix
- `docs:` - Documentation
- `refactor:` - Code refactoring
- `test:` - Tests
- `chore:` - Maintenance

### Code Style

- Prefer `just verify` before opening a PR (fmt, clippy with `--all-features`, build, tests)
- Or run `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test` yourself
- Follow existing patterns in the codebase
- Add tests for new functionality

## Pull Requests

1. Create a feature branch from `main`:
   ```bash
   git checkout main
   git pull upstream main
   git checkout -b feat/my-feature
   ```

2. Make your changes and commit

3. Push to your fork:
   ```bash
   git push origin feat/my-feature
   ```

4. Open a Pull Request against `main`

### PR Checklist

- [ ] `just verify` passes (or equivalent fmt / clippy `--all-features` / build / tests)
- [ ] Documentation updated if needed
- [ ] CHANGELOG.md updated for user-facing changes

## Reporting Issues

When reporting issues, include:

- Rust version (`rustc --version`)
- Rapina version
- Minimal reproduction case
- Expected vs actual behavior

## Questions?

Open a discussion on GitHub or reach out to the maintainers.

## License

By contributing, you agree that your contributions will be licensed under the MIT License.
