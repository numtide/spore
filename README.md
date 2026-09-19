# spore

One small boot image that becomes any Nix closure named in the cloud user-data.

The image contains only a kernel, a static musl initrd, and the Limine boot
loader. On the first boot, the initrd reads the user-data. It puts the
system on the disk and its kernel and initrd on the ESP, and starts it with
kexec. With the Rust /init, every later boot starts the bootstrap again: it
counts the boot and kexecs the system from the ESP. After a number of boots
without a good mark, it provisions again or stays in the bootstrap for
rescue (with a shell on the debug disk only). See
"Boot counting" below. The shell /init makes the system the default boot
entry instead.

## Build

The outputs are per platform. For hcloud on x86_64:

```sh
nix build .#hcloud-disk           # result/disk.img: raw GPT disk, 131 MiB
nix build .#hcloud-initrd         # result/initrd: zstd cpio, static musl only
nix build .#hcloud-kernel         # result/bzImage: KVM guest kernel, no modules
nix build .#hcloud-kernel-config  # the .config that the fragments resolve to
nix build .#hcloud-uki            # result/spore.efi: kernel, Rust initrd, cmdline in one PE
nix build .#hcloud-disk-uki       # the disk that chainloads that UKI on UEFI
nix flake check                   # QEMU boot per firmware, kernel config in sync
```

The build uses no VM. The x86_64 boot checks need the `kvm` system feature.

For hcloud on aarch64, build on an aarch64-linux builder:

```sh
nix build .#packages.aarch64-linux.hcloud-disk     # result/disk.img: raw GPT disk, 131 MiB
nix build .#packages.aarch64-linux.hcloud-kernel   # result/Image: arm64 KVM guest kernel
nix build .#checks.aarch64-linux.hcloud-boot-uefi  # QEMU virt with edk2, TCG
nix build .#packages.aarch64-linux.hcloud-disk-uki # the disk for cax: a UKI with a zboot kernel
```

`nix flake check` on an x86_64 host builds only the x86_64 checks. The
aarch64 check uses TCG, because hcloud cax servers have no `/dev/kvm`.

## Releases

To make a release, change `VERSION` on main. The nixbot `release` effect
runs on every push to main. It does nothing while a GitHub release of
`v$VERSION` exists. Otherwise it creates the tag on that commit and a
release with these files for each platform:

| File | Content |
| ---- | ------- |
| `spore-<version>-hcloud-<arch>.img.zst` | The raw disk to upload as an hcloud snapshot |
| `spore-<version>-hcloud-<arch>-debug.img.zst` | The same disk with the rescue shell |
| `spore-<version>-hcloud-<arch>.efi` | The bootstrap as a UKI |
| `spore-<version>-<arch>-linux` | The static `spore` binary (`spore pull`) |
| `SHA256SUMS` | The SHA-256 of each file |

`nix build .#release` builds the same files. It needs an aarch64-linux
builder.

## Repository layout

`common/` holds what all platforms share:

| File              | Content                                                    |
| ----------------- | ---------------------------------------------------------- |
| `init`            | The bootstrap /init: busybox sh                            |
| `dhcp-script`     | The udhcpc hook                                            |
| `initrd.nix`      | The initrd builder: `init`, static `tools`, extra `files`  |
| `default.nix`     | `shellInitrd`: the sh /init with its static musl tools; `rustInitrd` |
| `rescue-busybox.nix` | The busybox of the debug Rust initrd: only the emergency shell applets |
| `spore.nix`  | The Rust /init, with a smaller bundled SQLite              |
| `uki.nix`         | The UKI: ukify and systemd-stub with kernel, initrd, cmdline |
| `kernel-base.nix` | The base kernel config fragment                            |
| `kernel-config.nix` | Resolves a .config from the fragments                    |
| `kernel.nix`      | Builds a kernel from a committed .config; `zboot` adds EFI_ZBOOT |
| `disk.nix`        | The raw GPT disk builder; `uki = true` boots a UKI on UEFI  |
| `test.nix`        | The QEMU boot test                                         |

`platforms/<platform>-<arch>/` holds what is different for each target. The
flake exports it as `packages.<arch>-linux.<platform>-*` and
`checks.<arch>-linux.<platform>-*`. Each platform has:

| File            | Content                                                     |
| --------------- | ----------------------------------------------------------- |
| `kernel.nix`    | The platform kernel config fragment                         |
| `kernel.config` | The resolved .config. The `kernel-config` check keeps it in sync. |
| `userdata.sh`   | Defines `userdata_fetch`: gets the user-data from the metadata service |
| `default.nix`   | The kernel, initrd and disk, the boot command line, and the test settings: QEMU command, console, firmware lanes |

`targets/hcloud/` holds the lean NixOS target of the benchmarks. See "Lean
hcloud target" below. `targets/boot-good.nix` is the NixOS module that marks
a boot good for the boot counter, with its script `targets/boot-good.sh`.

The kernel of a platform is the arch defconfig and the config targets of the
platform, then the base fragment, then the platform fragment. The platform
fragment wins over the base fragment.

To change a fragment, run `nix build .#<platform>-kernel-config`. Then copy
`result` to `platforms/<platform>-<arch>/kernel.config`.

The bootstrap finds the disk through the ESP label. Thus it works on
virtio-blk (`vda`), SCSI (`sda`) and NVMe (`nvme0n1`) without a platform
setting.

Two platforms exist now. `hcloud-x86_64` (cx and cpx types):

- Kernel: KVM guest, virtio-blk, virtio-scsi, virtio-net. No modules.
- User-data: `http://169.254.169.254/hetzner/v1/userdata`, plain HTTP.
- Firmware: SeaBIOS on cx types, UEFI on cpx types. The test boots both.
- Output: a raw disk. Upload it as a snapshot: a helper server writes the
  image to its disk, then hcloud makes a snapshot of that disk.
- Console: VNC. VGA text on SeaBIOS, the EFI framebuffer on UEFI.

`hcloud-aarch64` (cax types, Ampere Altra):

- Kernel: arm64 `defconfig` plus `virt.config`. ACPI, PSCI, virtio-scsi,
  virtio-net. The bootstrap loads no modules.
- User-data: the same as x86_64.
- Firmware: UEFI only (EDK II). Limine starts from `EFI/BOOT/BOOTAA64.EFI`.
  Partition 2 stays empty, so the system is on partition 3 on both arches.
- Output: a raw disk. The nixos-images kexec installer hangs on cax (the
  Debian kernel stops after "Synchronizing SCSI cache"). So a Debian cax
  helper writes the image itself: the image goes to tmpfs, sysrq `u`
  remounts every file system read-only, dd writes the disk, and sysrq `o`
  powers off. Then hcloud makes a snapshot of that disk.
- Console: VNC shows the EFI framebuffer. The serial console is `ttyS0`
  (PCI 16550) on hcloud and `ttyAMA0` (PL011) in the QEMU test.
- arm64 kexec needs `SUSPEND`: CPU hotplug parks the secondary CPUs.

Later platforms: AWS Nitro (ENA, NVMe, IMDSv2 with a session token, AMI),
GCE (gVNIC, virtio-scsi, the `Metadata-Flavor` header, `disk.raw.tar.gz`),
ISO and netboot bundles.

## Disk layout

| Partition | Content                                                      |
| --------- | ------------------------------------------------------------ |
| 1         | ESP, FAT32, label `ESP`, 128 MiB: Limine, the bootstrap, the target |
| 2         | BIOS boot partition for Limine stage 2. Empty on aarch64.    |
| 3         | ext4, label `nixos`: made by the bootstrap on the first boot |

Limine boots on BIOS and on UEFI, on x86_64 and aarch64. Its only entry is
`/bootstrap` (the x86_64 UKI disk adds `/bootstrap-bios`, see "UKI" below). The Rust /init keeps the target in the `target/` directory of
the ESP: `kernel`, `initrd`, `cmdline` and the boot counter `tries`. The
shell /init writes a `/target` Limine entry above `/bootstrap`.

## User-data

The bootstrap reads JSON from the cloud user-data:

```json
{
  "system": "/nix/store/…-nixos-system-…",
  "substituters": ["https://cache.example.org"],
  "trusted-public-keys": ["cache.example.org-1:…"]
}
```

The system must contain `kernel`, `initrd`, `init` and `kernel-params`. A
NixOS toplevel has all four. The system must mount its root by label `nixos`
and its ESP by label `ESP`.

The Rust /init also reads these keys:

| Key          | Default       | Effect                                                   |
| ------------ | ------------- | -------------------------------------------------------- |
| `boot-tries` | `3`           | Boots of the system without a good mark, 1 to 9          |
| `fallback`   | `"provision"` | With no tries left: `"provision"` again, or `"rescue"`: a shell on the console, the disk stays as it is |
| `layout`     | none          | The partition layout; see "Partition layout"             |

## First boot

1. The initrd gets a DHCP lease.
1. It reads the user-data. Without user-data, it starts a shell.
1. It makes partition 3 in the free space and formats it as ext4.
1. It copies the system with `nix copy` from the first substituter. Nix
   checks the signatures against the trusted public keys.
1. It sets the system profile.
1. It copies the kernel and initrd of the system to the ESP. The shell
   /init writes the `/target` entry. The Rust /init writes the command line
   and the boot counter, with one try already taken.
1. It starts the system with kexec.

If a step fails, the initrd starts a shell on the console.

## Kernel command line

| Argument                  | Effect                                                         |
| ------------------------- | -------------------------------------------------------------- |
| `spore.userdata=URL` | Reads the user-data from URL, not from the platform `userdata_fetch`. |
| `spore.shell`        | Starts a shell before the network comes up.                    |
| `ip=…`                    | Rust /init only: keeps the network of the kernel `ip=` setting and runs no DHCP. |

## Measurements

The project was called nix-trampoline before. The hcloud snapshots named in
this README keep that name.

Sizes, x86_64: initrd 17.7 MB, kernel bzImage 7.7 MB, disk 131 MiB raw,
hcloud snapshot 0.025 GB. aarch64: initrd 16.6 MB, kernel Image 17.0 MB
(arm64 has no compressed Image), disk 131 MiB raw, hcloud snapshot
0.024 GB.

QEMU with KVM, from the start of /init (both firmwares): network 0.04 s,
disk 0.59 s, closure fetched 0.64 s, kexec 0.7 s. The test closure is tiny.
QEMU aarch64 with TCG and edk2: network 0.43 s, disk 2.70 s, closure
fetched 6.17 s, kexec 7.65 s.

hcloud, fsn1, 2026-09-19, from a signed HTTP cache in the same location.
Three creates per type. "API" is the time of `hcloud server create`. The
other times start when that call returns.

| Type  | API         | Bootstrap ping | Target running | Warm reboot to login |
| ----- | ----------- | -------------- | -------------- | -------------------- |
| cx23  | 12.2–14.7 s | 0.8–1.1 s      | 7.9–11.3 s     | 4.0–4.1 s            |
| cpx22 | 6.8–6.9 s   | 7.1–7.6 s      | 13.6–14.1 s    | 8.8 s                |
| cax11 | 7.0–8.0 s   | 5.2–6.3 s      | 18.1–21.0 s    | 12.9–13.1 s          |
| cax21 | 6.8–8.0 s   | 5.5–6.5 s      | 17.4–18.8 s    | 13.0 s               |

The x86_64 rows use snapshot nix-trampoline-v2 and a small NixOS system
with a custom kernel (410 MiB closure). The aarch64 rows use snapshot
nix-trampoline-aarch64-v1 and a minimal NixOS system with the stock
kernel (878 MiB closure, 316 MiB compressed). A cax warm reboot gives a
login after ~13 s. The target takes ~6.2 s of it (loader 0.4–0.7 s, kernel
0.4 s, initrd 3.1 s, userspace 2.3 s). The remaining ~6.8 s are the reset
and the UEFI firmware.

"Target running" means: ssh works and `/run/current-system` is the target.
The warm reboot is a sysrq reset. The time is from the reset to a root
login. In these rows, the second boot starts the `/target` entry and runs
no bootstrap. With boot counting, every boot runs the bootstrap: see "Boot
counting".

The stock aarch64 NixOS kernel (64 MB Image) and initrd (25.5 MB) use
90 MB of the 128 MiB ESP. The bootstrap files use 34 MB. Only 8 MB stay
free.

## Limits

- The bootstrap uses the first substituter only.
- The shell /init has a fixed layout. The Rust /init takes a repart layout, but ext4 only, and no encryption or RAID.
- The shell /init has no boot counting: a bad target stays the default entry.
- Boot counting costs 0.5–0.9 s on each warm boot (see "Boot counting").
- The aarch64 kernel has no softdog yet, so a hang there waits for a reset
  from outside. Its kernel.config needs a native aarch64 build.
- The ESP entry is Limine specific. No boot specification support.
- The kexec payload is trusted through the substituter signature only.

## Rust bootstrapper

`common/spore/` is one static musl binary that does the whole first
boot as /init. It replaces the shell /init, `nix`, `kexec`, `sfdisk`,
`mtools` and `jq`. The initrd keeps only a static `mke2fs` (there is no
Rust mkfs.ext4). The debug variant also has busybox for the emergency
and rescue shell: `nix build .#hcloud-disk-debug` (and `hcloud-initrd-debug`)
is the disk that hcloud gets, with that shell. Without it, an error prints
the reason and retries every 10 s, and a rescue waits in the bootstrap.

```sh
nix build .#spore            # result/bin/spore: 3.1 MB, static-pie
nix build .#hcloud-initrd-rust    # 2.0 MB zstd cpio (the shell initrd is 17.7 MB)
nix build .#hcloud-disk-rust      # the disk with the Rust initrd
```

| Module     | Content                                                           |
| ---------- | ----------------------------------------------------------------- |
| `net`      | DHCP client. Adds a host route to a gateway outside the subnet (hcloud leases a /32). |
| `fetch`    | Signed narinfo, 32 parallel downloads, xz and zstd, NarHash and NarSize checked on the stream |
| `store`    | NAR restore with canonical metadata; copied from ietsp `src/store` |
| `db`       | Registers the paths in the Nix database (schema 10)                |
| `repart`, `disk`, `gpt` | The partition layout (see below) and the GPT editor   |
| `esp`      | The target files and the boot counter on the FAT ESP; the kernel has no VFAT |
| `kexec`    | `kexec_file_load` and `reboot(LINUX_REBOOT_CMD_KEXEC)`              |

It logs each step with the time since it started, and the disk step with
its sub-steps in milliseconds. It writes the log to
`/var/log/spore.log` on the target. With `/bin/sh` in the initrd, a
failure starts a shell. Without it, the bootstrap tries again after 10 s.

`spore pull ROOT USERDATA.json` runs only the fetch and the
registration into `ROOT`. Use it to test a cache from any Linux host.

`packages.x86_64-linux.spore-aarch64` cross-compiles the binary for
aarch64 (2.8 MB, static) on an x86_64 host. `hcloud-aarch64` has
`initrd-rust` (2.0 MB), `disk-rust` and the Rust test lanes too. Build them
on an aarch64 builder. The native build also runs the unit tests.

A Debian cax41 with `nix-bin` from apt works as a temporary builder:
`nix build --store ssh-ng://root@<ip> --eval-store auto …`. It builds the
aarch64 disks, the Rust initrd and the four aarch64 checks (TCG) in about
10 minutes.

### Measurements

hcloud fsn1, 2026-09-19, snapshot nix-trampoline-rust-v3, the same cache
and 410 MiB NixOS system as the shell rows above. Three creates per type.

| Type  | API         | Bootstrap ping | Target running | Warm reboot to login |
| ----- | ----------- | -------------- | -------------- | -------------------- |
| cx23  | 12.2–14.7 s | 0.0–1.3 s      | 7.7–9.1 s      | 4.9–5.1 s            |
| cpx22 | 8.0–9.7 s   | 6.8–7.9 s      | 11.6–12.9 s    | 8.8–9.1 s            |

Inside the bootstrap, from the start of /init (the kernel starts /init
after 0.6–0.9 s):

| Step              | cx23        | cpx22       |
| ----------------- | ----------- | ----------- |
| network up        | 0.03–0.05 s | 0.02–0.31 s |
| disk ready        | 0.13–0.15 s | 0.11–0.38 s |
| closure fetched   | 2.50–3.30 s | 1.00–1.37 s |
| boot entry written | 2.62–3.51 s | 1.07–1.45 s |
| kexec             | 2.85–3.80 s | 1.12–1.50 s |

The cx23 CPU (Skylake) has no SHA-NI, so the fetch is CPU bound there.
`spore pull` of the same closure on a cx23 with Debian takes
1.0–1.1 s to ext4 and 0.8 s to tmpfs. The warm reboot does not run the
bootstrap. Its cx23 difference from the shell rows comes from the host:
the target initrd phase took 1.1–1.2 s there, not 0.75–0.77 s.

aarch64: snapshot nix-trampoline-rust-aarch64-v1, the stock-kernel system
of the shell rows above (878 MiB closure). All six creates ran at the same
time; the shell rows ran one after the other.

| Type  | API        | Bootstrap ping | Target running | Warm reboot to login |
| ----- | ---------- | -------------- | -------------- | -------------------- |
| cax11 | 9.6–9.7 s  | 5.2–6.5 s      | 15.9–17.3 s    | 12.6–13.4 s          |
| cax21 | 9.6–12.2 s | 4.2–6.0 s      | 14.9–18.4 s    | 12.9–13.0 s          |

The shell rows are 18.1–21.0 s (cax11) and 17.4–18.8 s (cax21). The kernel
starts /init after 0.26–0.29 s. From there:

| Step              | cax11       | cax21       |
| ----------------- | ----------- | ----------- |
| network up        | 0.02–0.05 s | 0.03–0.05 s |
| disk ready        | 1.54–1.70 s | 1.81–3.00 s |
| closure fetched   | 3.32–3.72 s | 3.14–4.33 s |
| boot entry written | 3.54–3.94 s | 3.42–4.57 s |
| kexec             | 3.96–4.34 s | 4.18–5.32 s |

The disk step took 1.5–3.0 s on cax, against 0.1–0.4 s on x86_64. All of
it was mke2fs, which discards the whole new partition by default. The
bootstrap now runs mke2fs with `-E nodiscard`: a disk made from a sparse
snapshot gets nothing from a discard. The lean-target round below shows the
effect.

### The disk step

hcloud fsn1, 2026-09-19, lean targets, 3 cx23, 3 cax11 and 3 cax21 created
at the same time per round. The bootstrap logs each sub-step:

| Sub-step               | cx23      | cax11 and cax21, before | cax11 and cax21, after |
| ---------------------- | --------- | ----------------------- | ---------------------- |
| GPT write + sync       | 1–6 ms    | 1–2 ms                  | 1–2 ms                 |
| BLKRRPART              | 7–20 ms, no retry | 16–26 ms, no retry | 15–27 ms, no retry  |
| partition node appears | 0 ms      | 0 ms                    | 0 ms                   |
| mke2fs                 | 15–47 ms  | 1325–1807 ms            | 19–24 ms               |
| mount                  | 1–5 ms    | 2–4 ms                  | 2–3 ms                 |

| Type  | Disk ready, before | after       | kexec after /init, before | after       | Target running, before | after       |
| ----- | ------------------ | ----------- | ------------------------- | ----------- | ---------------------- | ----------- |
| cax11 | 1.51–1.91 s        | 0.10–0.13 s | 2.78–3.24 s               | 1.37–1.55 s | 11.2–12.3 s            | 8.7–11.1 s  |
| cax21 | 1.39–1.72 s        | 0.11–0.40 s | 2.79–3.10 s               | 1.41–1.80 s | 10.8–12.5 s            | 9.2–11.0 s  |
| cx23  | 0.11–0.18 s        | 0.09–0.35 s | 2.28–2.47 s               | 1.01–1.08 s | 6.2–9.1 s              | 2.6–4.4 s   |

The cx23 servers of the "after" round were on faster hosts: their kernel,
target initrd and target userspace were all about 40% faster too, so the
cx23 gain is not from this change. The cx23 disk step stays the same.
Target running on cax varies by ±1 s between creates of one round.
Snapshots: nix-trampoline-rust-v4 (x86_64) and
nix-trampoline-rust-aarch64-v2.

## Boot counting

The Rust /init counts the boots of the target. After `boot-tries` boots
without a good mark, the machine falls back to the bootstrap. The shell
/init has no boot counting.

### Design

Limine cannot write files, so the bootstrap does the counting. Limine
starts only `/bootstrap`, on every boot. The bootstrap reads
`target/tries` on the ESP. The file holds "LEFT TOTAL".

- LEFT is more than 0: the bootstrap writes LEFT − 1 and syncs. Then it
  copies `target/kernel` and `target/initrd` to RAM and kexecs them with
  `target/cmdline`. This path uses no network, mounts nothing and writes
  no log file.
- LEFT is 0: the bootstrap reads the user-data. With `"fallback":
  "provision"` (the default), it provisions again, as on the first boot.
  It keeps the partitions, fetches only the missing paths, writes the
  target files again with a full counter, and kexecs the system. The
  user-data can name a new system. With `"fallback": "rescue"`, it starts
  a shell on the console and does not touch the disk.
- There is no complete target (no `target/tries`): the bootstrap
  provisions.

The provisioning takes one try for its own kexec. It removes `tries` and
syncs, writes the kernel, initrd and command line and syncs, and then
writes `tries`. After a power loss in these steps, the next boot finds no
`tries` and provisions again.

A good boot gives the tries back. `targets/boot-good.nix` adds
`spore-boot-good.service`, which runs `targets/boot-good.sh` after
`boot-complete.target`, in the manner of `systemd-bless-boot`. The script
writes "TOTAL TOTAL" to `target/tries`. It writes through the mount when
the target mounts the ESP. Otherwise it uses mtools on the raw partition,
because the platform kernel has no VFAT. A health check makes itself part
of the good mark with `requiredBy = [ "boot-complete.target" ]` and
`before = [ "boot-complete.target" ]`. `targets/hcloud` imports the module.

A target without the module never gives tries back. The bootstrap still
counts its boots and falls back after `boot-tries` boots, also when these
boots were healthy.

### What counts as a failed boot

| Failure | How the bootstrap finds it |
| ------- | -------------------------- |
| Kernel panic | The bootstrap adds `panic=10` to the target command line when the target sets no `panic=`. The kernel resets 10 s after the panic. The next bootstrap sees the try that it took. |
| Hang in the kernel, the initrd or the boot | `boot-good.nix` arms the software watchdog: `softdog.soft_active_on_boot=1 softdog.soft_margin=DEADLINE` (`spore.bootGood.deadline`, default 300 s). The good script stops it with the magic close character. Without that stop, softdog resets the machine at the deadline. |
| The target boots, but the good unit fails or never runs | The try stays taken. With the module, softdog resets the machine at the deadline. Without the module, the next reset from outside counts. |

The x86_64 platform kernel has softdog built in. A stock NixOS kernel has
it as a module, and `boot-good.nix` loads it in the initrd. A hang before
the softdog driver starts (early in the kernel) is not caught. The aarch64
kernel has no softdog yet, so its target sets the deadline to null. There,
a hang waits for a reset from outside.

### Rejected options

- Limine alone. Limine writes no file. Its only boot-to-boot state is in
  EFI variables: `remember_last_entry` (UEFI only, `CONFIG.md`), the Boot
  Loader Interface variables `LoaderEntryOneShot` and `LoaderEntryDefault`
  (`common/lib/bli.c`, read in `common/menu.c`), and `BootNext`
  (`common/protos/efi_boot_entry.c`). SeaBIOS (hcloud cx) has no EFI
  variables, none of these is a counter, and a target that panics cannot
  set them. `default_entry` is a fixed value in `limine.conf`. Source:
  Limine 12.6.0.
- A counter that the target decrements in its own initrd. A panic of the
  target kernel before its initrd is never counted.
- Count only until the first good boot, then start a `/target` entry
  directly, as systemd-boot does after `systemd-bless-boot`. Warm boots
  then cost nothing, but a later panic or hang of that target loops
  forever. The target must also rewrite `limine.conf`.
- UEFI one-shot: `/bootstrap` stays the default, and each good boot sets
  `LoaderEntryOneShot=/target` for the next boot. Limine erases the
  variable before it starts the entry, so a failed boot goes back to the
  bootstrap. This removes the warm-boot cost on UEFI only. SeaBIOS needs
  the bootstrap path anyway, the target must write EFI variables, and it is
  not known whether hcloud keeps EFI variables over a reset. A possible
  follow-up for cpx.
- A good mark on the root file system. The bootstrap then must mount ext4
  on each warm boot, with a journal replay after a reset. The ESP write is
  cheaper and does not depend on the root file system.

### Tests

`nix flake check` runs these Rust lanes, each on SeaBIOS and on OVMF:

| Check | Boots |
| ----- | ----- |
| `boot-rust-bios`, `boot-rust-uefi` | 1: provision; the target marks the boot good, `tries` is "3 3". 2: the bootstrap kexecs the target from the ESP 0.03–0.04 s after /init, without the network; `tries` is "3 3" again. |
| `boot-rust-*-panic` | `boot-tries` 2, the target panics (sysrq `c`). `tries` goes "1 2", then "0 2". Boot 3 provisions the same user-data again (0 paths fetched): "1 2". |
| `boot-rust-*-rescue` | `boot-tries` 1, `"fallback": "rescue"`, the target powers off without a good mark. Boot 2 stays in the bootstrap and does no kexec; `tries` stays "0 1". |
| `boot-debug-*-rescue` | The same on the debug disk: boot 2 starts the rescue shell. |
| `boot-rust-repart` | Removes `target/tries` after boot 1, so boot 2 provisions again; boot 3 kexecs from the ESP. |

The test targets run the same `boot-good.sh` as the NixOS module, with
busybox and a static mtools.

### hcloud

fsn1, 2026-09-19, the lean target with the module (274.0 MiB), snapshot
nix-trampoline-bootcount-x86_64-v1. The warm reboot is a sysrq reset, and
the time goes to a root login.

The same server boots both ways: 3 resets through the bootstrap, then a
`/target` Limine entry goes above `/bootstrap` (the old direct path), then
3 more resets. Three servers per type, all at the same time:

| Type  | Through the bootstrap | Direct `/target` entry | Cost, mean per server |
| ----- | --------------------- | ---------------------- | --------------------- |
| cx23  | 3.92–6.54 s           | 3.19–5.91 s            | +0.48–0.77 s          |
| cpx22 | 8.34–9.65 s           | 7.89–8.91 s            | +0.45–0.89 s          |

At the same time, snapshot nix-trampoline-rust-v4 with the target without
the module (3 servers per type, 9 resets each): cx23 3.40–4.02 s, cpx22
8.21–9.05 s. The spread between hosts is larger than the cost, so the
same-server columns give the cost.

Where the time goes, from a build that put time stamps on the kexec
command line (3 resets per type):

| Step                                   | cx23        | cpx22       |
| -------------------------------------- | ----------- | ----------- |
| Bootstrap kernel start to /init        | 0.55–0.60 s | 0.62–0.65 s |
| Read `tries`                           | 0.01 s      | 0.00 s      |
| Copy kernel and initrd from the ESP    | 0.10 s      | 0.05–0.08 s |
| Write `tries` and sync                 | < 0.01 s    | < 0.01 s    |
| `kexec_file_load` (SHA-256 of 27 MB)   | 0.18 s      | 0.04 s      |
| `reboot(KEXEC)` to target kernel start | 0.52–0.57 s | 0.30–0.31 s |

The last step includes the device shutdown, the SHA-256 check in the
purgatory, and the decompression of the target kernel (the direct path
also has that). The cx23 CPU has no SHA-NI. Limine loads 10.6 MB for the
bootstrap instead of 26.9 MB for the target, which gives some of it back.

A good target stays up past the deadline: uptime 345 s on one boot, so the
good script stopped softdog.

Fallback run: cx23, `boot-tries` 2, the lean target with a health check
that always fails and a 30 s deadline. Times from the create call:

| Time    | Boot | `tries` | What happened |
| ------- | ---- | ------- | ------------- |
| 20.1 s  | 1    | 1 2     | First boot: provision, kexec |
| 51.2 s  | 2    | 0 2     | softdog reset; the bootstrap took the last try |
| 84.0 s  | 3    | 1 2     | softdog reset; "the target used all 2 tries without a good boot"; provision again, 0 paths, kexec 0.28 s after /init |
| 116.3 s | 4    | 0 2     | softdog reset |
| 148.7 s | 5    | 1 2     | provision again, kexec 0.24 s after /init |

Each boot of the bad target lasts about 32.5 s: 30 s of deadline and the
boot. The loop goes on, because the same user-data names the same bad
system each time. `"fallback": "rescue"` stops it in the bootstrap.

## UKI, zboot and the initrd size

### UKI

`common/uki.nix` puts the kernel, the Rust initrd and the command line
into one PE: `ukify build` with the systemd-stub of nixpkgs. The flake
exports it as `packages.<arch>-linux.hcloud-uki`. It is the one artifact
for later ISO, netboot and AMI outputs.

`hcloud-disk-uki` boots it on UEFI. Limine stays the boot manager:

```
/bootstrap
    protocol: efi
    path: boot():/bootstrap.efi

/bootstrap-bios
    if_fw_type: BIOS
    protocol: linux
    …
```

Limine hides `efi` entries on BIOS and starts the first entry it can
boot. So x86_64 keeps `kernel` and `initrd` on the ESP for SeaBIOS (cx
types). The aarch64 disk has only `bootstrap.efi`. Limine passes no
command line, so systemd-stub uses the one in the UKI.

The ESP has an empty `/loader` directory. systemd-stub keeps its random
seed there. Without the directory, it logs "Failed to open random seed
file", and each logged error stalls the boot for 2.5 s (`log_wait` in
`src/boot/efi-log.c`). In QEMU with OVMF, that took the UKI from 1.3 s to
4.0–7.2 s after the reset.

### zboot on aarch64

`common/kernel.nix` with `zboot = true` takes the committed kernel.config,
adds `EFI_ZBOOT` and `KERNEL_ZSTD`, and builds `vmlinuz.efi`: 6.5 MB, not
the 17.0 MB Image. It is an EFI application that decompresses the kernel
and then runs the EFI stub.

- systemd-stub boots it. The stub loads the `.linux` section as a PE and
  calls its entry point. The QEMU lane and hcloud cax11 both boot it.
- Limine's linux protocol does not. `common/protos/linux_risc.c` wants
  `ARM\x64` (0x644d5241) at offset 0x38. zboot puts `LINUX_PE_MAGIC`
  (0x818223cd) there (`drivers/firmware/efi/libstub/zboot-header.S`).
  Limine 12.6.0 in QEMU: "PANIC: linux: invalid kernel image: kernel
  header magic does not match".
- arm64 `kexec_file_load` does not either: `arch/arm64/kernel/kexec_image.c`
  takes only the Image magic. So the target and the test system keep the
  Image. Only the UKI has the zboot kernel.

### Initrd

zstd -19 of each file alone, and of the whole cpio:

| File        | x86_64 before     | x86_64 after     | aarch64 before    | aarch64 after    |
| ----------- | ----------------- | ---------------- | ----------------- | ---------------- |
| /init       | 3.65 MB / 1.70 MB | 3.12 MB / 1.47 MB | 3.30 MB / 1.63 MB | 2.77 MB / 1.40 MB |
| busybox     | 1.41 MB / 0.74 MB | 0.52 MB / 0.26 MB | 1.66 MB / 0.77 MB | 0.65 MB / 0.28 MB |
| mke2fs      | 1.04 MB / 0.44 MB | 0.89 MB / 0.38 MB | 1.15 MB / 0.46 MB | 1.08 MB / 0.40 MB |
| CA bundle   | 0.47 MB / 0.15 MB | none             | 0.47 MB / 0.15 MB | none             |
| initrd      | 6.63 MB / 2.93 MB | 4.53 MB / 2.01 MB | 6.63 MB / 2.89 MB | 4.51 MB / 1.97 MB |

Since then the default initrd has no busybox: x86_64 1.81 MB (zstd), the
debug initrd 2.01 MB. The aarch64 initrds were not rebuilt (no arm builder).

- busybox (debug initrd only): `allnoconfig` plus the applets of the emergency shell (sh,
  setsid, cttyhack, and 60 tools such as ls, mount, ip, dmesg, vi). 65
  links, not ~400.
- /init already had fat LTO, `codegen-units = 1`, `panic = "abort"` and
  strip. The symbols before (x86_64, `nm --size-sort`): ring 380 KB
  (with the 151 KB P-256 table), SQLite 366 KB, std 252 KB, rustls 248 KB,
  unwind tables 275 KB, the spore 143 KB, zstd 132 KB, ureq and url
  100 KB, fatfs 94 KB, xz 46 KB, webpki 44 KB, IDNA 34 KB plus its ICU
  tables.
- SQLite: `LIBSQLITE3_FLAGS` undoes FTS3, FTS5, RTREE, DBSTAT, STAT4,
  JSON, column metadata and load_extension of the bundled build: -406 KB,
  -194 KB compressed. `db.rs` only inserts rows; `nix path-info` and
  `nix store verify` read the database.
- `idna_adapter` 1.0.0 is the ASCII-only IDNA of the url crate: -123 KB,
  -43 KB compressed. A substituter host name must be ASCII or punycode.
- mke2fs: `-Os`, `-ffunction-sections -fdata-sections` and `--gc-sections`.
- The CA bundle: the Rust /init never reads it. ureq 2 without
  `native-certs` uses `webpki-roots`, the Mozilla roots inside the binary,
  like the NSS roots in `cacert`. `spore pull` from
  https://cache.nixos.org works with this binary. The shell initrd keeps
  the bundle for static nix.
- Not taken: `opt-level = "z"` (-26 KB compressed) also applies to the NAR
  restore and the fetch loop, and is not measured; `-C force-unwind-tables=no`
  gives only -9 KB compressed, because std comes precompiled with its unwind
  tables. SHA-256, xz and zstd stay at opt-level 3.

### Sizes

| Artifact             | x86_64  | aarch64                      |
| -------------------- | ------- | ---------------------------- |
| Kernel               | 7.7 MB bzImage | 17.0 MB Image, 6.5 MB zboot |
| Initrd               | 2.0 MB  | 2.0 MB                       |
| UKI                  | 9.9 MB  | 19.2 MB with the Image, 8.6 MB with zboot |
| systemd-stub         | 166 KB  |                              |

### hcloud

fsn1, 2026-09-19, the lean target with the boot-good module from
farmtest-cache. Three servers per row, all rows of a type one after the
other. "Target running" is from the return of the create call, "reboot"
is a sysrq reset to a root login, on the third server of each row. "After
the reset" is the new log line of the first boot: the CPU counter at
/init, so firmware, Limine and the bootstrap kernel together.

| Snapshot                                 | Type  | Target running | Reboot      | After the reset |
| ---------------------------------------- | ----- | -------------- | ----------- | --------------- |
| nix-trampoline-bootcount-x86_64-v1 (main) | cx23  | 5.4–7.6 s      | 5.81–6.24 s |                 |
| linux entry, small initrd                | cx23  | 4.8–10.3 s     | 5.43–5.74 s | 3.17–5.47 s     |
| UKI disk (BIOS: linux entry)             | cx23  | 5.0–7.2 s      | 7.21–7.43 s | 3.68–5.61 s     |
| nix-trampoline-bootcount-x86_64-v1 (main) | cpx22 | 10.3–12.0 s    | 9.04–9.07 s |                 |
| linux entry, small initrd                | cpx22 | 8.3–11.1 s     | 8.83–9.23 s | 7.98–11.00 s    |
| UKI                                      | cpx22 | 11.4–12.5 s    | 9.72–9.83 s | 8.91–9.30 s     |
| nix-trampoline-rust-aarch64-v2 (main, direct `/target` entry) | cax11 | 9.8–10.2 s | 9.18–9.42 s |      |
| linux entry, small initrd                | cax11 | 10.4–10.7 s    | 10.06–10.32 s | 7.58–7.71 s   |
| UKI with the Image                       | cax11 | 10.5–11.0 s    | 10.18–10.33 s | 7.58–7.79 s   |
| UKI with zboot                           | cax11 | 9.3–11.5 s     | 9.94–10.18 s | 7.53–7.61 s    |

The cx23 UKI row had a slow host (kernel 0.9–1.0 s, not 0.7 s): on cx23
the UKI disk boots the same linux entry as the other disk. The spread
between hosts is larger than the effect, so the same server boots each
entry in turn: the target copies another limine.conf to the ESP before
each reset, three rounds.

| Type  | Servers | linux entry  | UKI          | kernel EFI stub (`efi` entry for the bzImage, `initrd=`) | UKI cost, per server |
| ----- | ------- | ------------ | ------------ | ------------ | -------------------- |
| cpx22 | 4       | 9.17–9.58 s  | 9.37–9.76 s  | 9.47–9.71 s (2 servers) | +0.16, +0.17, +0.17, +0.20 s |
| cax11 | 2       | 9.99–10.16 s | 9.96–10.11 s |              | -0.04, -0.01 s       |

On cax11 the zboot UKI took 9.93–10.18 s in the same rounds (+0.07 and
-0.02 s per server against the linux entry).

So:

- x86_64 UEFI: the UKI costs about 0.17 s. The kernel's own EFI stub costs
  even more (+0.25 s), so the cost is the EFI stub boot path of the x86
  kernel, not systemd-stub. Limine's linux protocol enters the kernel
  without it. The hcloud x86_64 disk stays `hcloud-disk-rust`.
- aarch64: the linux entry, the UKI and the zboot UKI are equal within
  0.1 s. `hcloud-disk-uki` with zboot is the cax disk: it puts 8.6 MB on
  the ESP instead of 19.0 MB.
- The smaller initrd does not show in these times: the loaders read 1 MB
  less, which is below the spread.

Snapshots: nix-trampoline-uki-x86_64-v1 is `hcloud-disk-rust` (the "linux
entry, small initrd" rows), nix-trampoline-uki-aarch64-v1 is
`packages.aarch64-linux.hcloud-disk-uki` (the "UKI with zboot" row).

## Lean hcloud target

`targets/hcloud/` is a small NixOS system for the benchmarks. The flake
exports it as `packages.<arch>-linux.hcloud-target`. It boots the kernel of
its platform, which has every driver built in, so the target initrd loads
no modules. It has a slim systemd, nix without ICU or AWS, sshd without
PAM, no setuid wrappers, and a short core package list. The bootstrap owns
the disk layout, so the target has no growpart or repart.

| Arch    | Closure   | Paths | Kernel | Initrd |
| ------- | --------- | ----- | ------ | ------ |
| x86_64  | 274.0 MiB | 287   | 7.4 MB | 19 MB  |
| aarch64 | 332.3 MiB | 283   | 17 MB  | 19 MB  |

The x86_64 row includes `targets/boot-good.nix`: mtools, the script and the
unit add 0.2 MiB and 3 paths. The aarch64 row is from before the module.

The aarch64 target uses 36 MB of the ESP. The stock-kernel system used
90 MB.

hcloud fsn1, 2026-09-19, the same cache as above. All creates of a table
ran at the same time. x86_64 uses snapshot nix-trampoline-rust-v3:

| Type  | API        | Bootstrap ping | Target running | Warm reboot to login | Target boot |
| ----- | ---------- | -------------- | -------------- | -------------------- | ----------- |
| cx23  | 9.7–14.7 s | 0.0–0.8 s      | 5.8–7.5 s      | 4.3–4.5 s            | 2.8–3.1 s   |
| cpx22 | 9.7 s      | 5.0–6.0 s      | 9.9–11.0 s     | 8.7–8.9 s            | 2.1–2.2 s   |

From the start of /init, the closure is fetched after 1.68–1.97 s (cx23)
and 0.80–1.45 s (cpx22). kexec follows after 2.01–2.34 s and 0.92–1.56 s.
"Target boot" is the `systemd-analyze` total of the target.

The first aarch64 table uses the shell snapshot nix-trampoline-aarch64-v1, because the
Rust aarch64 snapshot did not exist yet. Compare with the stock-kernel
rows above (target running 18.1–21.0 s and 17.4–18.8 s, reboot ~13 s):

| Type  | API   | Bootstrap ping | Target running | Warm reboot to login | Target boot |
| ----- | ----- | -------------- | -------------- | -------------------- | ----------- |
| cax11 | 8.0 s | 6.0–6.5 s      | 13.7–14.0 s    | 9.2–9.3 s            | 2.6–2.8 s   |
| cax21 | 8.0 s | 6.0–6.5 s      | 13.3–14.0 s    | 9.0–9.2 s            | 2.4–2.5 s   |

With the Rust snapshot nix-trampoline-rust-aarch64-v1 (six creates at the
same time):

| Type  | API        | Bootstrap ping | Target running | Warm reboot to login | Target boot |
| ----- | ---------- | -------------- | -------------- | -------------------- | ----------- |
| cax11 | 9.7 s      | 5.0–5.8 s      | 11.8–12.0 s    | 9.2–9.3 s            | 2.6 s       |
| cax21 | 9.7–12.3 s | 4.5–4.7 s      | 13.0–13.6 s    | 9.1–9.2 s            | 2.4–2.7 s   |

From the start of /init, the closure is fetched after 2.27–2.66 s (cax11)
and 1.95–3.84 s (cax21). kexec follows after 2.76–3.14 s and 2.81–4.77 s.

No unit fails on the lean aarch64 target. It suppresses
`systemd-boot-random-seed.service`, which failed on the stock target.

## Partition layout

The bootstrap uses systemd-repart semantics, as a subset in Rust. The
layout comes from the first of these that exists:

1. `layout` in the user-data.
1. The repart.d of the target: `<system>/etc/repart.d/*.conf`, as NixOS
   writes it from `systemd.repart.partitions`. The bootstrap fetches only
   the store paths on that path into RAM, before it partitions.
1. The default: ext4, label `nixos`, over the free space.

The `layout` in the user-data has the repart.d keys, as JSON:

```json
{
  "system": "/nix/store/…",
  "substituters": ["…"],
  "trusted-public-keys": ["…"],
  "layout": {
    "wipe": false,
    "partitions": [
      {"Type": "root", "Label": "nixos", "Format": "ext4", "SizeMaxBytes": "20G", "MountPoint": "/"},
      {"Type": "linux-generic", "Label": "data", "Format": "ext4", "MountPoint": "/var"}
    ]
  }
}
```

Supported keys: `Type` (esp, xbootldr, swap, home, srv, var, tmp,
user-home, linux-generic, root, usr, root-x86-64, root-arm64, usr-x86-64,
usr-arm64, or a type GUID), `Label`, `UUID`, `Format` (ext4 only),
`SizeMinBytes`, `SizeMaxBytes`, `Weight`, `PaddingMinBytes`,
`PaddingMaxBytes`, `PaddingWeight` and `MountPoint`. Sizes take the
suffixes K, M, G, T (base 1024).

The rules follow repart.c of systemd 258:

- Each existing partition, in table order, matches the first definition of
  the same type that has no partition yet. The label does not take part in
  the match. Existing partitions that match no definition stay as they are.
- The bootstrap never changes a matched partition and never formats it
  again. The data on it stays. The exception: a matched partition with no
  data in its first 64 KiB gets its file system (a run that stopped before
  mkfs).
- New partitions go first-fit into the free areas, smallest area first.
  Each one needs its minimum size plus its padding, in 4 KiB grains. The
  minimum is 10 MiB, or 32 MiB for ext4, or `SizeMinBytes`.
- The free space goes by weight in three phases: a partition that needs
  more than its share gets its minimum, a partition that takes less gets
  its maximum, and the rest share by weight.
- A new partition without a label gets the type name as its label.
- The ext4 label is the partition label.

Differences from systemd-repart:

- Matched partitions do not grow into free space after them. That needs a
  file system resize.
- `"wipe": true` removes every partition except the ESP and the BIOS boot
  partition before the match. The ESP holds the bootstrap.
- `Encrypt`, `Verity`, `CopyFiles`, `CopyBlocks`, `MakeDirectories`,
  `Subvolumes`, `Minimize` and other formats than ext4 stop the bootstrap
  with "not supported yet", but only when the partition must be made. Other
  keys are ignored with a message.

The bootstrap mounts the ext4 partitions with a `MountPoint` under /mnt
and puts the store on the one for `/`. It skips other file systems with a
message: a vfat ESP with `MountPoint=/boot` is normal. Without any
`MountPoint`, it uses the first root type, or else the first ext4
definition. The target must mount its own file systems, for example by
label.
