%define __spec_install_post %{nil}
%define __os_install_post %{_dbpath}/brp-compress
%define debug_package %{nil}
%{!?_unitdir:%global _unitdir %{_prefix}/lib/systemd/system}

Name:           oc-rsync
Summary:        Pure-Rust implementation of rsync-compatible client and daemon functionality.
Version:        @@VERSION@@
Release:        @@RELEASE@@%{?dist}
License:        GPL-3.0-or-later
URL:            https://github.com/oferchen/rsync
Group:          Applications/System
Source0:        %{name}-%{version}.tar.gz
Requires:       glibc, libgcc

BuildRoot:      %{_tmppath}/%{name}-%{version}-%{release}-root

%description
Pure-Rust implementation of rsync-compatible client and daemon functionality.

%prep
%setup -q

%install
rm -rf %{buildroot}
mkdir -p %{buildroot}
cp -a * %{buildroot}

%clean
rm -rf %{buildroot}

%post
%systemd_post oc-rsyncd.service

%preun
%systemd_preun oc-rsyncd.service

%postun
%systemd_postun_with_restart oc-rsyncd.service

%files
%defattr(-,root,root,-)
%{_bindir}/oc-rsync
%{_unitdir}/oc-rsyncd.service
%config(noreplace) %{_sysconfdir}/oc-rsyncd/oc-rsyncd.conf
%config(noreplace) %{_sysconfdir}/oc-rsyncd/oc-rsyncd.secrets
%config(noreplace) %{_sysconfdir}/default/oc-rsyncd
