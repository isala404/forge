# Releasing Forge

Forge normally releases the Rust crate, Node package, and Python package in lockstep.
The metadata-only reservation packages under `bindings/` remain at `0.0.1` and are
disabled by default in the release workflow.

### 1.0.0 distribution exception

The crates.io name already contains an unrelated, yanked, immutable
`forgelib 1.0.0`. The repository's `v1.0.0` release therefore publishes
only npm and PyPI artifacts plus the GitHub Release. Every 1.0.0 workflow dispatch
must set `publish_crates=false`; do not unyank or attempt to overwrite the crates.io
artifact. Rust users can pin the Git tag until 1.0.1 restores lockstep publication:

```toml
forgelib = { git = "https://github.com/isala404/forge", tag = "v1.0.0" }
```

## 1. Prepare the release commit

1. Work from a clean `main` branch with all required CI checks passing.
2. Confirm the `main` ruleset requires the release CI checks and GitHub private
   vulnerability reporting is enabled.
3. Confirm `Cargo.toml`, `bindings/node/package.json`, and
   `bindings/python/pyproject.toml` carry the same version.
4. Confirm `CHANGELOG.md` has a dated section for that version and no release notes
   remain accidentally under `Unreleased`.
5. Run the local release gates:

   ```bash
   cargo fmt --all --check
   cargo clippy --all-targets --all-features -- -D warnings
   cargo test
   cargo audit --deny warnings
   cargo audit --file bindings/node/Cargo.lock --deny warnings
   cargo audit --file bindings/python/Cargo.lock --deny warnings
   cargo semver-checks check-release --release-type minor \
     --baseline-rev forgelib-v0.0.1
   cargo package
   npm test --prefix bindings/node
   (cd bindings/node && npm pack --dry-run --ignore-scripts)
   ```

6. Run the PostgreSQL and cross-language suites using a disposable database:

   ```bash
   TEST_DATABASE_URL=postgres://postgres:forge@localhost:5432/forge_dev \
     cargo test --features pg-tests -- --test-threads=4
   TEST_DATABASE_URL=postgres://postgres:forge@localhost:5432/forge_dev \
     bash tools/conformance/run-all.sh
   ```

## 2. Push, verify CI, then tag the exact commit

Release tags use the repository's existing `vX.Y.Z` convention. Never move or reuse
a published tag. Push the release commit before creating the tag, then wait for both
`CI` and `Drift Guards` to succeed for that exact SHA. If the branch ruleset does not
require those checks, this manual wait is mandatory.

```bash
git push origin main
gh run list --repo isala404/forge --commit "$(git rev-parse HEAD)"
# Watch the CI and Drift Guards runs to successful completion before continuing.

git fetch origin main
test "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)"
git tag -s v1.0.0 -m "Forge 1.0.0"
git push origin v1.0.0
```

## 3. Run a dry release

Dispatch `.github/workflows/release.yml` from the tag with `dry_run=true`. For the
1.0.0 exception, disable crates.io explicitly, keep npm and PyPI enabled, and leave
every metadata-only target disabled.

```bash
gh workflow run release.yml \
  --repo isala404/forge \
  --ref v1.0.0 \
  -f version=1.0.0 \
  -f release_tag=v1.0.0 \
  -f dry_run=true \
  -f publish_crates=false \
  -f publish_npm=true \
  -f publish_pypi=true
```

Inspect every artifact and job before running the same dispatch with
`dry_run=false`. Keep the three publish flags exactly the same. A successful 1.0.0
real run publishes the Node and Python packages and then creates the GitHub Release
from the matching changelog section. Starting with 1.0.1, re-enable
`publish_crates=true` and return all three packages to lockstep.

## 4. Verify the published release

- Install `forgelib==1.0.0` into clean Python 3.9 and current Python environments.
- Install `forgelib@1.0.0` on every supported Node platform.
- Create a clean Rust project pinned to the `v1.0.0` Git tag.
- Exercise embedded Postgres and an external PostgreSQL 17 instance.
- Confirm npm, PyPI, and the GitHub Release come from the tagged commit. Confirm the
  crates.io 1.0.0 artifact remains yanked and is not presented as this release.
- Prepare 1.0.1 as the first fully synchronized Rust/npm/PyPI patch release.

Registry versions and Git tags are immutable. Fix a bad release with a new patch
version; never overwrite an existing artifact or move its tag.
