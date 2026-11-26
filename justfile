# rapina task runner — install just: https://github.com/casey/just
# Agents and humans: `just verify` is the local gate that matches the CI Check job.
#
# Prefer rustup's cargo so rust-toolchain.toml (nightly-2026-02-15) wins over Homebrew.

export PATH := env_var("HOME") + "/.cargo/bin:" + env_var_or_default("PATH", "")

# List available recipes
default:
    @just --list

# Docs/examples version pins match crate version (same check as CI)
version-check:
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION=$(grep '^version' rapina/Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
    MINOR="${VERSION%.*}"
    BAD=$(grep -rn 'rapina = { version = "' docs/ rapina/examples/ --exclude-dir="blog" \
      --include="*.md" --include="*.toml" \
      | grep -v "\"$VERSION\"\|\"$MINOR\"\|\"\.\.\.\"" || true)
    if [ -n "$BAD" ]; then
      echo "Outdated version references found (expected $VERSION or $MINOR):"
      echo "$BAD"
      exit 1
    fi
    echo "version-check ok ($VERSION)"

# Local CI Check job: version pins, fmt, clippy (all features), build, tests
verify: version-check
    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo build --locked --all-targets
    cargo test --locked --all

# Alias — same as verify
check: verify

# Faster local loop (default features only). Not a substitute for verify/CI.
verify-quick: version-check
    cargo fmt --all -- --check
    cargo clippy --all-targets -- -D warnings
    cargo test --locked --all

# Run tests only
test:
    cargo test --locked --all

# Run doc tests
test-doc:
    cargo test --locked --doc --all-features

# Format code
fmt:
    cargo fmt --all

# Bump versions, update docs/examples, open release PR
# Usage: just release 0.12.0
release VERSION:
    #!/usr/bin/env bash
    set -euo pipefail

    VERSION="{{VERSION}}"
    echo "Preparing release v${VERSION}..."

    MINOR="${VERSION%.*}"

    # Bump Cargo.toml package versions
    sed -i '' 's/^version = ".*"/version = "{{VERSION}}"/' rapina/Cargo.toml rapina-macros/Cargo.toml rapina-cli/Cargo.toml

    # Bump rapina-macros cross-reference in rapina/Cargo.toml
    sed -i '' 's/rapina-macros = { version = "[^"]*"/rapina-macros = { version = "{{VERSION}}"/' rapina/Cargo.toml

    # Bump version references in docs and examples (excluding blog history)
    find docs/content/docs/ rapina/examples/ \( -name "*.md" -o -name "*.toml" \) \
        | xargs sed -i '' \
            -e "s/rapina = { version = \"[0-9][^\"]*\"/rapina = { version = \"{{VERSION}}\"/g" \
            -e "s/^rapina = \"[0-9][^\"]*\"/rapina = \"{{VERSION}}\"/g"

    # Regenerate Cargo.lock
    cargo check --workspace

    # Refresh snapshots that embed the crate version (e.g. llms.txt footer)
    INSTA_UPDATE=always cargo test --locked -p rapina --lib introspection::llms

    # Create branch, commit, push, open PR
    git checkout -b release_{{VERSION}}
    git add rapina/Cargo.toml rapina-macros/Cargo.toml rapina-cli/Cargo.toml Cargo.lock CHANGELOG.md
    git add docs/ rapina/examples/
    git add rapina/src/introspection/snapshots/
    git commit -m "Bump version to {{VERSION}}"
    git push origin release_{{VERSION}}
    gh pr create \
        --title "Release v{{VERSION}}" \
        --base main \
        --head release_{{VERSION}} \
        --body "Bumps all version references to {{VERSION}}. Merge this PR then push the \`v{{VERSION}}\` tag to trigger the release."

    echo ""
    echo "PR opened. Once merged, tag and push:"
    echo "  git tag v{{VERSION}} && git push origin v{{VERSION}}"

# Push the release tag after the version bump PR is merged
# Usage: just tag 0.12.0
tag VERSION:
    git checkout main
    git pull origin main
    git tag v{{VERSION}}
    git push origin v{{VERSION}}
    echo "Tag v{{VERSION}} pushed — CI will handle the GitHub release and crates.io publish."
