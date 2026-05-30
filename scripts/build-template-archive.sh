#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
ARCHIVE_DIR="${ROOT_DIR}/crates/forge/generated"
ARCHIVE_PATH="${ARCHIVE_DIR}/examples.tar"
STAGING_DIR="$(mktemp -d)"

cleanup() {
  rm -rf "${STAGING_DIR}"
}
trap cleanup EXIT

mkdir -p "${ARCHIVE_DIR}"

copy_template() {
  local src="$1"
  local dest="$2"

  mkdir -p "${dest}"

  rsync -a \
    --exclude='.git/' \
    --exclude='pg_data/' \
    --exclude='target/' \
    --exclude='node_modules/' \
    --exclude='.svelte-kit/' \
    --exclude='build/' \
    --exclude='dist/' \
    --exclude='playwright-report/' \
    --exclude='test-results/' \
    --exclude='.forge-dev-integration.log' \
    --exclude='skills/' \
    --exclude='package-lock.json' \
    --exclude='.env' \
    --exclude='frontend/target/' \
    "${src}/" "${dest}/"
}

for framework_dir in "${ROOT_DIR}"/examples/with-*; do
  [ -d "${framework_dir}" ] || continue
  framework_name="$(basename "${framework_dir}")"

  for template_dir in "${framework_dir}"/*; do
    [ -d "${template_dir}" ] || continue
    template_name="$(basename "${template_dir}")"
    copy_template "${template_dir}" "${STAGING_DIR}/${framework_name}/${template_name}"
  done
done

# Read version from workspace Cargo.toml
VERSION=$(grep -m1 '^version = ' "${ROOT_DIR}/Cargo.toml" | head -1 | sed 's/version = "\(.*\)"/\1/')
echo "Rewriting template deps to published version ${VERSION}"

# Rewrite backend Cargo.toml: workspace dep -> published version
for cargo_toml in "${STAGING_DIR}"/with-*/*/Cargo.toml; do
  [ -f "$cargo_toml" ] || continue
  sed -i "s/forge = { workspace = true }/forge = { version = \"${VERSION}\", package = \"forgex\" }/" "$cargo_toml"
done

# Rewrite Svelte frontend package.json: file: path -> exact published version
for pkg in "${STAGING_DIR}"/with-*/*/frontend/package.json; do
  [ -f "$pkg" ] || continue
  jq --arg v "=${VERSION}" '
    if .dependencies["@forge-rs/svelte"] then .dependencies["@forge-rs/svelte"] = $v
    elif .devDependencies["@forge-rs/svelte"] then .devDependencies["@forge-rs/svelte"] = $v
    else . end
  ' "$pkg" > "$pkg.tmp" && mv "$pkg.tmp" "$pkg"
done

# Rewrite Dioxus frontend Cargo.toml: path dep -> exact published version
for cargo in "${STAGING_DIR}"/with-*/*/frontend/Cargo.toml; do
  [ -f "$cargo" ] || continue
  sed -i "s|forge-dioxus = { path = \"[^\"]*\" }|forge-dioxus = { version = \"=${VERSION}\" }|g" "$cargo"
done

# Rewrite otel service: local build context -> published GHCR image with
# Forge's pre-provisioned dashboards. The version is read from the Dockerfile
# so it stays in sync with what docker-otel-lgtm.yml publishes; falling back
# to the upstream image would lose the dashboards.
OTEL_VERSION=$(grep -oE 'FROM grafana/otel-lgtm:[^\s]+' docker/otel-lgtm/Dockerfile \
  | head -n1 | sed 's|FROM grafana/otel-lgtm:||')
OTEL_IMAGE="ghcr.io/isala404/forge/otel-lgtm:${OTEL_VERSION:-latest}"
for compose in "${STAGING_DIR}"/with-*/*/docker-compose.yml; do
  [ -f "$compose" ] || continue
  sed -i "s|build: ../../../docker/otel-lgtm|image: ${OTEL_IMAGE}|g" "$compose"
done

# Ensure frontend .env files exist in templates
for dir in "${STAGING_DIR}"/with-*/*/frontend; do
  [ -d "$dir" ] || continue
  [ -f "$dir/.env" ] || echo 'PUBLIC_API_URL=http://localhost:9081' > "$dir/.env"
done

tar -cf "${ARCHIVE_PATH}" -C "${STAGING_DIR}" .
echo "Wrote ${ARCHIVE_PATH}"
