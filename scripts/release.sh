#!/usr/bin/env bash
# Prepare a release: pick the next version with git-cliff (or take one as an
# argument), bump the workspace version, regenerate CHANGELOG.md, run tests and
# commit. Publishing and tagging are printed as next steps, not run.
set -euo pipefail

cd "$(dirname "$0")/.."

current=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
next=${1:-$(git cliff --bumped-version)}
next=${next#v}

if [[ "$next" == "$current" ]] && git rev-parse -q --verify "refs/tags/v$next" >/dev/null; then
  echo "v$next is already tagged; nothing to release" >&2
  exit 1
fi

echo "Releasing $current -> $next"

# Workspace version and the versions on internal path dependencies.
sed -i.bak \
  -e "/^\[workspace.package\]/,/^\[/s/^version = \"$current\"/version = \"$next\"/" \
  -e "/^ddbg-[a-z]* = { path/s/version = \"$current\"/version = \"$next\"/" \
  Cargo.toml
rm Cargo.toml.bak

cargo check --workspace --quiet # refreshes Cargo.lock
git cliff --tag "v$next" -o CHANGELOG.md
cargo test --workspace --quiet

jj commit -m "chore(release): v$next"

cat <<EOF

Release v$next prepared. Next:

  git cliff --latest --strip header   # review release notes
  cargo publish --workspace           # crates.io (Rust 1.90+)
  jj bookmark set master -r @-
  jj git push --bookmark master
  git tag v$next \$(jj log -r @- --no-graph -T commit_id)
  git push origin v$next
EOF
