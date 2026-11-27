#!/usr/bin/env bash
# Publishes the agito crates to crates.io, in dependency order, then tags the release.
#
# Usage: scripts/publish.sh [--dry-run] [--skip-tests] [--integration] [--yes]
#
#   --dry-run      package and verify everything, upload nothing, create no tag
#   --skip-tests   skip fmt, clippy, the codegen check and unit tests
#   --integration  also run the browser tests (needs Chrome/Chromium)
#   --yes          do not ask for confirmation before uploading
#
# Requires `cargo login` beforehand (or CARGO_REGISTRY_TOKEN in the environment).

set -euo pipefail

CRATES=(agito-protocol agito-macros agito agito-cli)

dry_run=false
skip_tests=false
integration=false
assume_yes=false

for arg in "$@"; do
    case "$arg" in
        --dry-run) dry_run=true ;;
        --skip-tests) skip_tests=true ;;
        --integration) integration=true ;;
        --yes | -y) assume_yes=true ;;
        -h | --help)
            sed -n '2,/^[^#]/p' "$0" | grep '^#' | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown option: $arg (see --help)" >&2
            exit 2
            ;;
    esac
done

step() { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }
fail() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

cd "$(dirname "$0")/.."

step "Checking the working tree"
if [[ -n "$(git status --porcelain)" ]]; then
    fail "the working tree has uncommitted changes; commit or stash them first"
fi
branch="$(git rev-parse --abbrev-ref HEAD)"
echo "branch: $branch"

version="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p}' Cargo.toml)"
[[ -n "$version" ]] || fail "could not read the version from [workspace.package] in Cargo.toml"
tag="v$version"
echo "version: $version"

if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    fail "tag $tag already exists; bump the version in Cargo.toml first"
fi

for crate in "${CRATES[@]}"; do
    # crates.io answers 200 for a version that exists and 404 for one that does not.
    status="$(curl -s -o /dev/null -w '%{http_code}' -A 'agito-publish-script' \
        "https://crates.io/api/v1/crates/$crate/$version")"
    if [[ "$status" == "200" ]]; then
        fail "$crate $version is already on crates.io; bump the version first"
    fi
done

if ! $skip_tests; then
    step "Formatting"
    cargo fmt --all --check

    step "Clippy"
    cargo clippy --workspace --all-targets \
        --features agito/integration,agito-samples/integration -- -D warnings

    step "Generated CDP bindings are current"
    cargo xtask codegen --check

    step "Unit tests"
    cargo test --workspace

    if $integration; then
        step "Browser tests"
        cargo test -p agito -p agito-samples \
            --features agito/integration,agito-samples/integration
    fi
fi

package_args=()
for crate in "${CRATES[@]}"; do
    package_args+=(-p "$crate")
done

step "Packaging and verifying ${CRATES[*]}"
cargo publish --dry-run "${package_args[@]}"

if $dry_run; then
    step "Dry run finished; nothing was uploaded"
    exit 0
fi

if ! $assume_yes; then
    echo
    read -r -p "Publish ${CRATES[*]} $version to crates.io? Published versions cannot be replaced. [y/N] " answer
    [[ "$answer" =~ ^[Yy]$ ]] || fail "aborted"
fi

step "Publishing"
cargo publish "${package_args[@]}"

step "Tagging $tag"
git tag -a "$tag" -m "agito $version"
git push origin "$tag"

step "Published agito $version"
for crate in "${CRATES[@]}"; do
    echo "  https://crates.io/crates/$crate/$version"
done
