# K3s on the persistent SD rootfs

The `k3s` prebuilt package contains K3s v1.37.1+k3s1 for RISC-V and the small
offline image set built by `build-airgap.sh`. The upstream K3s release does not
publish a RISC-V executable; `build-riscv64.sh` builds its pinned source using a
RISC-V musl toolchain, Go 1.26.8 and libseccomp 2.6.1. Pass a separate work
directory and the full compiler path; `gperf`, `pkg-config`, Git, Make, curl and
zstd must be available on the Linux builder. The official build scripts also
fetch their pinned CNI sources. Build timestamps follow the upstream scripts.

`k3s-start` is launched in the background by the SD rootfs startup script. It
uses an Ethernet IPv4 address if DHCP completes within eight seconds; otherwise
it creates the upstream-documented dummy interface and default route on
203.0.113.254/31. Network selection is repeated after reboot. This fallback
supports an isolated single node; external and cross-board networking require
separate validation. State and container images live under
`/var/lib/rancher/k3s`, with logs in `/var/log/k3s.log`.

CoreDNS uses the offline archive's compatibility tag for the official RISC-V
CoreDNS binary. Traefik, metrics-server, ServiceLB, local storage provisioning
and Helm are disabled. `k3s-check` waits for Node Ready and CoreDNS, then checks
HTTP between two ordinary Pods, their ClusterIP Service, and cluster DNS. It
leaves the `tianzhi-check` namespace for inspection and reboot verification.

References: [K3s source release](https://github.com/k3s-io/k3s/releases/tag/v1.37.1%2Bk3s1),
[air-gap setup](https://docs.k3s.io/installation/airgap),
[server configuration](https://docs.k3s.io/cli/server).
