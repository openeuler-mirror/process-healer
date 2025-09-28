Name:           healer
Version:        0.1.0
Release:        1%{?dist}
Summary:        Process self-healing daemon leveraging eBPF for monitoring and recovery

License:        MulanPSL-2.0
URL:            https://github.com/XqiLiu/healer
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  gcc
BuildRequires:  make
BuildRequires:  rust
BuildRequires:  cargo
BuildRequires:  clang
BuildRequires:  llvm
# bpf-linker is required by aya_build to link eBPF objects; install via cargo if not available
# BuildRequires: rust-bpf-linker

Requires(post): systemd
Requires(preun): systemd
Requires(postun): systemd

%description
Healer is a high-performance daemon leveraging eBPF for reliable, low-overhead
monitoring and automatic recovery of critical processes to ensure service continuity.

It provides pluggable monitors (PID / Network / eBPF), broadcast event bus, and
recovery with circuit breaker and backoff.

%prep
%setup -q -n %{name}-%{version}

%build
# Ensure cargo-installed binaries (like bpf-linker) are in PATH
export PATH="$HOME/.cargo/bin:$PATH"
# Install bpf-linker if missing (best-effort)
if ! command -v bpf-linker >/dev/null 2>&1; then
  cargo install --locked bpf-linker || true
fi

# Build only the main daemon binary in release mode
cargo build --release -p healer

%install
mkdir -p %{buildroot}%{_bindir}
install -m 0755 target/release/healer %{buildroot}%{_bindir}/healer

# Config
mkdir -p %{buildroot}%{_sysconfdir}/healer
install -m 0644 config.yaml %{buildroot}%{_sysconfdir}/healer/config.yaml

# systemd unit
mkdir -p %{buildroot}%{_unitdir}
install -m 0644 packaging/systemd/healer.service %{buildroot}%{_unitdir}/healer.service

# Log and runtime directories (created on install if not present)
mkdir -p %{buildroot}/var/log/healer
mkdir -p %{buildroot}/var/run/healer

%post
%systemd_post healer.service
mkdir -p /var/log/healer || true
mkdir -p /var/run/healer || true
chown root:root /var/log/healer /var/run/healer || true
chmod 755 /var/log/healer /var/run/healer || true

%preun
%systemd_preun healer.service

%postun
%systemd_postun_with_restart healer.service

%files
%doc README.md
%license LICENSE
%dir %{_sysconfdir}/healer
%config(noreplace) %{_sysconfdir}/healer/config.yaml
%{_bindir}/healer
%{_unitdir}/healer.service
%dir /var/log/healer
%dir /var/run/healer

%changelog
* Tue Sep 24 2025 XqiLiu - 0.1.0-1
- Initial RPM packaging for healer daemon
