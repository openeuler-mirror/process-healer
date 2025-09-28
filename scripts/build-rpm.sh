#!/usr/bin/env bash
set -euo pipefail

# Build an RPM for healer using rpmbuild -tb on a source tarball.

if [[ ${1:-} == "--help" ]]; then
  cat <<'EOF'
Usage: bash scripts/build-rpm.sh [--skip-dep-check]

Build the healer RPM from the current working copy.
Steps:
  1. Create a clean source tarball healer-<version>.tar.gz
  2. Invoke rpmbuild -ba packaging/rpm/healer.spec

Options:
  --skip-dep-check   Do not perform local RPM build dependency preflight.

Notes:
  Even if you have rust/cargo/clang via rustup or custom install in $HOME, rpmbuild
  validates BuildRequires against INSTALLED RPM PACKAGES. So a binary existing in PATH
  (e.g. ~/.cargo/bin/cargo) does not satisfy 'BuildRequires: cargo'. Install the distro
  packages or adjust the spec if you intentionally want to bypass them.
EOF
  exit 0
fi

ROOT_DIR=$(cd "$(dirname "$0")/.." && pwd)
PACKAGE_NAME=healer
PACKAGE_VERSION=$(grep '^version' "$ROOT_DIR/healer/Cargo.toml" | head -n1 | awk -F '"' '{print $2}' || echo "0.1.0")
PKGDIR="$ROOT_DIR/packaging/rpm"

# ---------------------------------------------
# Detect OS family; provide early warning on Debian/Ubuntu where BuildRequires
# cannot be satisfied because they are RPM package names.
# ---------------------------------------------
if [[ -f /etc/os-release ]]; then
  # shellcheck disable=SC1091
  . /etc/os-release
  os_id_like=${ID_LIKE:-}
  os_id=${ID:-}
  if ! command -v rpm >/dev/null 2>&1; then
    echo "[Warn] This system (ID=$os_id) does not have 'rpm' installed. Install rpm-build or run inside a Fedora/CentOS/RHEL container." >&2
  fi
  if [[ $os_id == "ubuntu" || $os_id == debian || $os_id_like == *"debian"* ]]; then
    cat >&2 <<EOF
[Info] Detected Debian/Ubuntu based system ($PRETTY_NAME).
       The spec's BuildRequires expect *RPM packages* (cargo, rust, clang, llvm, gcc, make).
       Even if you installed toolchains via apt or rustup, rpmbuild dependency resolution
       uses the RPM database only, so BuildRequires will fail here.

Recommended approaches:
  1. Use a Fedora/CentOS/Rocky container to build:
       docker run --rm -v "$PWD":"/src" -w /src fedora:latest bash scripts/build-rpm.sh
  2. Or install a full RPM toolchain on Ubuntu (adds alien env):
       sudo apt install rpm rpm-build clang llvm build-essential
       (Still cannot satisfy 'BuildRequires: cargo rust' unless you create RPM packages.)
  3. Or create a .deb package instead (e.g. with cargo-deb) for Ubuntu distribution.

To proceed anyway the script will continue, but rpmbuild likely fails at dependency phase.
EOF
  fi
fi

# ---------------------------------------------
# Preflight: verify BuildRequires are installed as RPM packages
# ---------------------------------------------
if [[ ${1:-} != "--skip-dep-check" ]]; then
  REQUIRED_RPM_PKGS=(cargo rust clang llvm gcc make rpm-build)
  missing_pkgs=()
  for p in "${REQUIRED_RPM_PKGS[@]}"; do
    if ! rpm -q "$p" &>/dev/null; then
      missing_pkgs+=("$p")
    fi
  done

  if ((${#missing_pkgs[@]})); then
    echo "[Preflight] Missing required RPM packages (BuildRequires will fail): ${missing_pkgs[*]}" >&2
    echo "Detected PATH versions (if any):" >&2
    for bin in cargo rustc clang llvm-ar gcc make; do
      if command -v "$bin" &>/dev/null; then
        echo "  - $bin -> $(command -v $bin)" >&2
      fi
    done
    cat >&2 <<EOF

Why this happens:
  rpmbuild checks *installed RPM packages* names, not just presence of executables.
  If you installed Rust via rustup (~/.cargo), it will NOT satisfy 'BuildRequires: cargo'.

Fix options:
  * Fedora / CentOS Stream / RHEL (with CRB enabled):
      sudo dnf install -y cargo rust clang llvm gcc make rpm-build
  * RHEL 9 derivatives: ensure CodeReady Builder / CRB repo enabled.
  * To bypass (NOT recommended), rerun: bash scripts/build-rpm.sh --skip-dep-check
    and optionally edit healer.spec to remove or conditionalize BuildRequires.

Aborting now to avoid a confusing rpmbuild failure. Use --skip-dep-check to override.
EOF
    exit 1
  fi
fi

echo "==> Preparing source tarball ${PACKAGE_NAME}-${PACKAGE_VERSION}.tar.gz"
TMP_SRC=$(mktemp -d)
trap 'rm -rf "$TMP_SRC"' EXIT

# Create a clean source tree directory name as %{name}-%{version}
SRC_ROOT="$TMP_SRC/${PACKAGE_NAME}-${PACKAGE_VERSION}"
mkdir -p "$SRC_ROOT"

# Copy all project files (excluding target/ by default)
rsync -a --exclude 'target/' --exclude '.git/' --exclude 'target-*' "$ROOT_DIR/" "$SRC_ROOT/"

pushd "$TMP_SRC" >/dev/null
tar czf "${PACKAGE_NAME}-${PACKAGE_VERSION}.tar.gz" "${PACKAGE_NAME}-${PACKAGE_VERSION}"
popd >/dev/null

echo "==> Running rpmbuild"
mkdir -p "$HOME/rpmbuild/SOURCES"
cp "$TMP_SRC/${PACKAGE_NAME}-${PACKAGE_VERSION}.tar.gz" "$HOME/rpmbuild/SOURCES/"

rpmbuild -ba "$PKGDIR/${PACKAGE_NAME}.spec" \
  --define "_topdir $HOME/rpmbuild" \
  --define "_sourcedir $HOME/rpmbuild/SOURCES" \
  --define "_builddir $HOME/rpmbuild/BUILD" \
  --define "_rpmdir $HOME/rpmbuild/RPMS" \
  --define "_srcrpmdir $HOME/rpmbuild/SRPMS" \
  --define "_specdir $ROOT_DIR/packaging/rpm"

echo "==> Done. Find RPMs under $HOME/rpmbuild/RPMS"
