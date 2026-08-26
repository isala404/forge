#!/usr/bin/env bash
# setup-disk.sh — reclaim and stretch disk in the Claude Code cloud sandbox.
#
# The sandbox root fs reports ~252G but fences off ~215G via ext4 reserved
# blocks (resv_strict + resuid/resgid=nobody), leaving ~30G usable. That cap
# cannot be changed from inside the VM. This script maximises what's left.
#
# Idempotent. Safe to re-run. Intended as an environment setup script.

set -uo pipefail
shopt -s nullglob

log() { printf '[disk] %s\n' "$*"; }
avail_kb() { df --output=avail / | tail -1 | tr -d ' '; }
gb() { awk -v k="${1:-0}" 'BEGIN{printf "%.1fG", k/1048576}'; }

START=$(avail_kb)
log "start: $(gb "$START") available"

# ---------------------------------------------------------------------------
# 1. Toolchains this stack never uses. The container is ephemeral, so this
#    re-runs each session by design. Edit to taste.
# ---------------------------------------------------------------------------
for p in /opt/rbenv /opt/ruby-* /opt/gradle-* /usr/lib/jvm /opt/node20* /opt/node21*; do
  [ -e "$p" ] || continue
  sz=$(du -sxm "$p" 2>/dev/null | cut -f1)
  rm -rf -- "$p" 2>/dev/null && log "removed $p (${sz:-?}M)"
done

# ---------------------------------------------------------------------------
# 2. Regenerable caches
# ---------------------------------------------------------------------------
rm -rf /root/.npm/_cacache /root/.cache/pip /root/.cache/ms-playwright 2>/dev/null
command -v go >/dev/null && go clean -cache -modcache -testcache 2>/dev/null
log "cleared npm/pip/go caches"

# ---------------------------------------------------------------------------
# 3. Rust: the single biggest lever. Debug info is typically 50-70% of a
#    target/ dir. Env vars are used because they outrank both config.toml and
#    a project's own Cargo.toml [profile] blocks.
# ---------------------------------------------------------------------------
mkdir -p /root/.cargo
cat > /root/.cargo/config.toml <<'TOML'
# Shared across projects so deps aren't rebuilt per-checkout.
[build]
target-dir = "/root/.cargo/shared-target"
incremental = false

[profile.dev]
debug = 0
incremental = false

[profile.dev.package."*"]
debug = 0

[profile.release]
debug = 0
incremental = false
strip = "symbols"
TOML

cat > /etc/profile.d/rust-disk.sh <<'ENVSH'
# Hard overrides — these beat any per-project Cargo.toml profile.
export CARGO_TARGET_DIR=/root/.cargo/shared-target
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_DEV_INCREMENTAL=false
export CARGO_PROFILE_RELEASE_DEBUG=0
export CARGO_PROFILE_RELEASE_STRIP=symbols
export CARGO_INCREMENTAL=0
ENVSH
chmod +x /etc/profile.d/rust-disk.sh
log "rust configured: debug=0, incremental=off, shared target dir"

# ---------------------------------------------------------------------------
# 4. Docker shares the same 30G via /var/lib/docker
# ---------------------------------------------------------------------------
if docker info >/dev/null 2>&1; then
  docker system prune -af --filter "until=24h" >/dev/null 2>&1
  docker builder prune -af >/dev/null 2>&1
  log "pruned docker images + build cache"
fi

# ---------------------------------------------------------------------------
# 5. Helpers for mid-session use
# ---------------------------------------------------------------------------
cat > /usr/local/bin/disk-report <<'RPT'
#!/usr/bin/env bash
# True picture: df's Size column is meaningless here, Available is real.
echo "== available =="
df / | awk 'NR==2{
  printf "  %.1fG free of a ~%.1fG session allowance (%.0f%% used)\n", \
         $4/1048576, ($3+$4)/1048576, $3*100/($3+$4)
  printf "  (device shows %.0fG total; ~%.0fG reserved and unreachable)\n", \
         $2/1048576, ($2-$3-$4)/1048576
}'
echo "== biggest consumers =="
du -shx /root/.cargo/shared-target /var/lib/docker /root/.rustup /opt/pw-browsers 2>/dev/null | sort -rh
echo "== docker =="
docker system df 2>/dev/null | head -5
RPT
chmod +x /usr/local/bin/disk-report

cat > /usr/local/bin/disk-reclaim <<'RCL'
#!/usr/bin/env bash
# Aggressive mid-session reclaim.
set -uo pipefail
before=$(df --output=avail / | tail -1)
docker system prune -af >/dev/null 2>&1
docker builder prune -af >/dev/null 2>&1
command -v cargo-sweep >/dev/null && cargo sweep -r -t 1 / 2>/dev/null
rm -rf /root/.cargo/registry/src /root/.cargo/shared-target/*/incremental 2>/dev/null
command -v go >/dev/null && go clean -cache -testcache 2>/dev/null
after=$(df --output=avail / | tail -1)
awk -v b="$before" -v a="$after" 'BEGIN{printf "reclaimed %.1fG -> %.1fG free\n",(a-b)/1048576,a/1048576}'
RCL
chmod +x /usr/local/bin/disk-reclaim

END=$(avail_kb)
log "done: $(gb "$END") available (recovered $(gb $((END-START))))"
log "helpers: disk-report, disk-reclaim"
