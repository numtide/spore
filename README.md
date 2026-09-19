# spore

One small boot image that becomes any Nix closure named in the cloud user-data.

The image holds a kernel, a static initrd and the Limine boot loader. On the
first boot, the initrd reads the user-data. It lays out the disk, copies the
system from a signed binary cache, puts the kernel and initrd of the system
on the ESP, and starts the system with kexec. On every later boot, the
bootstrap counts the boot and kexecs the system from the ESP. After a number
of boots without a good mark, it provisions again, or it stays in the
bootstrap for rescue.

spore is under the MIT license. See `LICENSE`.

## Build

The outputs are per platform. For hcloud on x86_64:

```sh
nix build .#hcloud-disk           # result/disk.img: the release disk, raw GPT, 131 MiB
nix build .#hcloud-disk-debug     # the same disk with the rescue shell
nix build .#hcloud-initrd         # result/initrd: spore as /init and a static mke2fs
nix build .#hcloud-kernel         # result/bzImage: KVM guest kernel, no modules
nix build .#hcloud-kernel-config  # the .config that the fragments resolve to
nix build .#hcloud-uki            # result/spore.efi: kernel, initrd and cmdline in one PE
nix build .#hcloud-target         # the lean NixOS target of the measurements
nix build .#spore                 # result/bin/spore: the static bootstrap binary
nix flake check                   # QEMU boot tests, kernel config in sync
```

The build uses no VM. The x86_64 boot tests need the `kvm` system feature.
`nix flake check` on an x86_64 host runs only the x86_64 checks.

For hcloud on aarch64, build on an aarch64-linux builder:

```sh
nix build .#packages.aarch64-linux.hcloud-disk        # the release disk: a UKI with a zboot kernel
nix build .#packages.aarch64-linux.hcloud-disk-debug  # the same disk with the rescue shell
nix build .#packages.aarch64-linux.hcloud-target
nix build .#checks.aarch64-linux.hcloud-boot-uefi
```

The aarch64 tests use TCG, because hcloud cax servers have no `/dev/kvm`.
`packages.x86_64-linux.spore-aarch64` cross-compiles the binary for aarch64
on an x86_64 host. The native build also runs the unit tests.

A Debian cax41 with `nix-bin` from apt works as a temporary builder:
`nix build --store ssh-ng://root@<ip> --eval-store auto …`.

## Repository layout

`common/` holds what all platforms share:

| File                 | Content |
| -------------------- | ------- |
| `spore/`             | The bootstrap: one static musl binary, run as /init |
| `spore.nix`          | Builds it, with a smaller bundled SQLite |
| `default.nix`        | `initrd`: spore as /init, with busybox in the debug variant |
| `initrd.nix`         | The initrd builder: `init`, static `tools`, extra `files` |
| `rescue-busybox.nix` | The busybox of the debug initrd: only the applets of the rescue shell |
| `kernel-base.nix`    | The base kernel config fragment |
| `kernel-config.nix`  | Resolves a .config from the fragments |
| `kernel.nix`         | Builds a kernel from a committed .config; `zboot` adds `EFI_ZBOOT` |
| `uki.nix`            | The UKI: ukify and systemd-stub with the kernel, initrd and cmdline |
| `disk.nix`           | The raw GPT disk; `uki = true` boots a UKI on UEFI |
| `test.nix`           | The QEMU boot tests |

`platforms/<platform>-<arch>/` holds what is different for each target. The
flake exports it as `packages.<arch>-linux.<platform>-*` and
`checks.<arch>-linux.<platform>-*`. Each platform has:

| File            | Content |
| --------------- | ------- |
| `kernel.nix`    | The platform kernel config fragment |
| `kernel.config` | The resolved .config. The `kernel-config` check keeps it in sync. |
| `default.nix`   | The kernel, the initrds, the disks (`disk` is the release disk), the target and the test settings |

`targets/boot-good.nix` is the NixOS module that marks a boot good for the
boot counter. `targets/boot-good.sh` is its script. `targets/hcloud/` is the
lean NixOS target of the measurements.

The kernel of a platform is the arch defconfig and the config targets of the
platform, then the base fragment, then the platform fragment. The platform
fragment wins. To change a fragment, run `nix build .#<platform>-kernel-config`.
Then copy `result` to `platforms/<platform>-<arch>/kernel.config`.

## Platforms

`hcloud-x86_64` (cx and cpx types):

- Kernel: KVM guest, virtio-blk, virtio-scsi, virtio-net, softdog. No modules.
- Firmware: SeaBIOS on cx types, UEFI on cpx types. The tests boot both.
- Release disk: `hcloud-disk`, the same as `hcloud-disk-linux`. Limine starts the kernel with its linux
  protocol on both firmwares.
- Console: VNC. VGA text on SeaBIOS, the EFI framebuffer on UEFI.

`hcloud-aarch64` (cax types, Ampere Altra):

- Kernel: arm64 `defconfig` plus `virt.config`, with ACPI, PSCI, virtio-scsi
  and virtio-net. The bootstrap loads no modules.
- Firmware: UEFI only (EDK II). Limine starts from `EFI/BOOT/BOOTAA64.EFI`.
- Release disk: `hcloud-disk`, the same as `hcloud-disk-uki`. Limine chainloads a UKI with a zboot
  kernel.
- Console: VNC shows the EFI framebuffer. The serial console is `ttyS0`
  (PCI 16550) on hcloud and `ttyAMA0` (PL011) in the QEMU test.

On both, the user-data comes from `http://169.254.169.254/hetzner/v1/userdata`
over plain HTTP. The bootstrap finds the disk through the ESP label, so it
works on virtio-blk (`vda`), SCSI (`sda`) and NVMe (`nvme0n1`).

Later platforms: AWS Nitro (ENA, NVMe, IMDSv2 with a session token, AMI),
GCE (gVNIC, virtio-scsi, the `Metadata-Flavor` header, `disk.raw.tar.gz`),
ISO and netboot bundles.

## Disk layout

| Partition | Content |
| --------- | ------- |
| 1 | ESP, FAT32, label `ESP`, 128 MiB: Limine, the bootstrap, the target |
| 2 | BIOS boot partition for Limine stage 2. Empty on aarch64. |
| 3 | ext4, label `nixos`: made by the bootstrap on the first boot |

The disk is 131 MiB. The rest of the server disk stays free for the
bootstrap. The system is on partition 3 on both arches.

Limine is the boot manager on BIOS and UEFI, on x86_64 and aarch64. It
starts only `/bootstrap`. On a UKI disk, `/bootstrap` is an `efi` entry for
`/bootstrap.efi`. The x86_64 UKI disk also has `/bootstrap-bios`, a linux
entry, because Limine hides `efi` entries on BIOS and starts the first entry
it can boot.

The bootstrap keeps the target in the `target/` directory of the ESP:
`kernel`, `initrd`, `cmdline` and the boot counter `tries`.

## User-data

The bootstrap reads the `spore` section of the cloud user-data, a JSON
document. The rest of the document belongs to the target: the bootstrap
does not read it, and the target can read the same user-data for its own
keys.

```json
{
  "spore": {
    "version": 1,
    "system": {
      "x86_64-linux": "/nix/store/…-nixos-system-…",
      "aarch64-linux": "/nix/store/…-nixos-system-…"
    },
    "substituters": ["https://cache.example.org"],
    "trusted-public-keys": ["cache.example.org-1:…"]
  },
  "my-service": { "join-token": "…" }
}
```

| Key                   | Default       | Effect |
| --------------------- | ------------- | ------ |
| `version`             | required      | The format of the section. Only `1` exists. |
| `system`              | required      | A store path, or a map from `<arch>-linux` to a store path, so one user-data serves every arch |
| `substituters`        | required      | Binary caches, asked in order |
| `trusted-public-keys` | required      | The keys that must sign the closure |
| `boot-tries`          | `3`           | Boots of the system without a good mark, 1 to 9 |
| `fallback`            | `"provision"` | With no tries left: `"provision"` again, or `"rescue"`: stay in the bootstrap, the disk stays as it is |
| `layout`              | none          | The partition layout; see "Partition layout" |

An unknown key inside `spore` is an error, so a typo does not pass as a
default.

The system must contain `kernel`, `initrd`, `init` and `kernel-params`. A
NixOS toplevel has all four. The system must mount its root by label `nixos`.

## First boot

1. The initrd gets a DHCP lease.
1. It reads the user-data.
1. It lays out the disk (see "Partition layout") and formats the new
   partitions as ext4.
1. It copies the closure of the system from the first substituter that has
   each path. The narinfo must have a signature by a trusted key. The NAR
   hash and size are checked on the stream.
1. It registers the paths in the Nix database and sets the system profile.
1. It copies the kernel, the initrd and the command line of the system to
   the ESP, with the boot counter. The first boot takes one try.
1. It writes its log to `/var/log/spore.log` on the target and starts the
   system with kexec.

If a step fails, the release disk prints the reason and tries again after
10 s. The debug disk starts a shell on the console.

## Kernel command line

| Argument          | Effect |
| ----------------- | ------ |
| `spore.userdata=URL` | Reads the user-data from URL, not from the platform URL |
| `spore.shell`     | Starts a shell before the network comes up (debug disk) |
| `ip=…`            | Keeps the network of the kernel `ip=` setting and runs no DHCP |

## The bootstrap

`common/spore/` is one static musl binary that does the whole first boot as
/init. The initrd holds only this binary and a static `mke2fs`, because there
is no Rust mkfs.ext4.

| Module     | Content |
| ---------- | ------- |
| `net`      | DHCP client. Adds a host route to a gateway outside the subnet (hcloud leases a /32). |
| `fetch`    | Signed narinfo, 32 parallel downloads, xz and zstd, NarHash and NarSize checked on the stream |
| `store`    | NAR restore with canonical metadata, through one 256 KiB window per download; copied from ietsp `src/store` (relicensed under MIT) |
| `db`       | Registers the paths in the Nix database (schema 10) |
| `repart`, `disk`, `gpt` | The partition layout and the GPT editor |
| `esp`      | The target files and the boot counter on the FAT ESP; the kernel has no VFAT |
| `kexec`    | `kexec_file_load` and `reboot(LINUX_REBOOT_CMD_KEXEC)` |

It logs each step with the time since /init started, the disk step with its
sub-steps in milliseconds, the time from the reset to /init (from the CPU
counter), and its peak memory before the kexec.

`spore pull ROOT USERDATA.json` runs only the fetch and the registration
into `ROOT`. Use it to test a cache from any Linux host.

The debug disk (`hcloud-disk-debug`) has the same bootstrap and a busybox
with only the applets of the emergency shell. There, a failure starts a
shell, and `"fallback": "rescue"` starts the rescue shell. The release disk
has no shell: a failure retries every 10 s, and a rescue waits in the
bootstrap.

## Boot counting

The bootstrap counts the boots of the target. After `boot-tries` boots
without a good mark, the machine falls back to the bootstrap.

Limine starts `/bootstrap` on every boot. The bootstrap reads `target/tries`
on the ESP. The file holds "LEFT TOTAL".

- LEFT is more than 0: the bootstrap writes LEFT − 1 and syncs. Then it
  copies `target/kernel` and `target/initrd` to RAM and kexecs them with
  `target/cmdline`. This path uses no network, mounts nothing and writes no
  log file.
- LEFT is 0: the bootstrap reads the user-data. With `"fallback":
  "provision"`, it provisions again, as on the first boot. It keeps the
  partitions, fetches only the missing paths, writes the target files again
  with a full counter, and kexecs the system. The user-data can name a new
  system. With `"fallback": "rescue"`, it stays in the bootstrap and does
  not touch the disk.
- There is no complete target (no `target/tries`): the bootstrap
  provisions.

The provisioning removes `tries` and syncs, writes the kernel, initrd and
command line and syncs, and then writes `tries`. After a power loss in these
steps, the next boot finds no `tries` and provisions again.

A good boot gives the tries back. `targets/boot-good.nix` adds
`spore-boot-good.service`, which runs `targets/boot-good.sh` after
`boot-complete.target`, in the manner of `systemd-bless-boot`. The script
writes "TOTAL TOTAL" to `target/tries`. It writes through the mount when the
target mounts the ESP. Otherwise it uses mtools on the raw partition. A
health check makes itself part of the good mark with
`requiredBy = [ "boot-complete.target" ]` and
`before = [ "boot-complete.target" ]`. `targets/hcloud` imports the module.

A target without the module never gives tries back. The bootstrap still
counts its boots and falls back after `boot-tries` boots, also when these
boots were healthy.

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

`nix flake check` runs these lanes, each on every firmware of the platform:

| Check | Boots |
| ----- | ----- |
| `boot-*` | 1: provision; the target marks the boot good, `tries` is "3 3". 2: the bootstrap kexecs the target from the ESP without the network; `tries` is "3 3" again. |
| `boot-*-panic` | `boot-tries` 2, the target panics (sysrq `c`). `tries` goes "1 2", then "0 2". Boot 3 provisions the same user-data again (0 paths fetched): "1 2". |
| `boot-*-rescue` | `boot-tries` 1, `"fallback": "rescue"`, the target powers off without a good mark. Boot 2 stays in the bootstrap and does no kexec; `tries` stays "0 1". |
| `boot-debug-*-rescue` | The same on the debug disk: boot 2 starts the rescue shell. |
| `boot-uki-*`, `boot-linux-*` | The good lane on the disk variant that is not the release disk: the UKI disk on x86_64, the linux entry disk on aarch64. |
| `boot-repart` | Removes `target/tries` after boot 1, so boot 2 provisions again; boot 3 kexecs from the ESP. |

The test targets run the same `boot-good.sh` as the NixOS module, with
busybox and a static mtools.

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
  "spore": {
    "version": 1,
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

## Releases

To make a release, change `VERSION` on main. The nixbot `release` effect
runs on every push to main. It does nothing while a GitHub release of
`v$VERSION` exists. Otherwise it creates the tag on that commit and a
release with these files for each platform:

| File | Content |
| ---- | ------- |
| `spore-<version>-hcloud-<arch>.img.zst` | The release disk, to upload as an hcloud snapshot |
| `spore-<version>-hcloud-<arch>-debug.img.zst` | The debug disk |
| `spore-<version>-hcloud-<arch>.efi` | The bootstrap as a UKI |
| `spore-<version>-<arch>-linux` | The static `spore` binary (`spore pull`) |
| `SHA256SUMS` | The SHA-256 of each file |

`nix build .#release` builds the same files. It needs an aarch64-linux
builder.

## Upload to hcloud

hcloud makes a snapshot from the disk of a server, so a helper server
writes the image to its own disk:

- x86_64: a cx23 kexecs into the NixOS installer, which runs from RAM. It
  runs `blkdiscard -f` on the disk, writes the image with
  `dd conv=sparse`, and powers off.
- aarch64: the NixOS kexec installer hangs on cax (the Debian kernel stops
  after "Synchronizing SCSI cache"). So a Debian cax11 copies the image,
  util-linux `blkdiscard`, GNU `dd` and their libraries to tmpfs. Sysrq `u`
  remounts every file system read-only. Then `blkdiscard -f`,
  `dd conv=sparse` and sysrq `o`. busybox `blkdiscard` opens the disk with
  `O_EXCL` and fails on a mounted disk, and busybox `dd` has no
  `conv=sparse`.

Without the discard, the snapshot keeps the old data of the helper disk.

Then `hcloud server create-image --type snapshot` makes the snapshot. A
server from the snapshot needs user-data.

## Lean hcloud target

`targets/hcloud/` is a small NixOS system for the measurements. The flake
exports it as `packages.<arch>-linux.hcloud-target`. It boots the kernel of
its platform, which has every driver built in, so the target initrd loads
no modules. It has a slim systemd, nix without ICU or AWS, sshd without PAM,
no setuid wrappers, and a short package list. It gets the root ssh keys from
the metadata service. The bootstrap owns the disk layout, so the target has
no growpart or repart. It imports `targets/boot-good.nix`.

## Design notes

- **Limine alone cannot count boots.** Limine writes no file. Its only
  state from one boot to the next is in EFI variables:
  `remember_last_entry` (UEFI only), the Boot Loader Interface variables
  `LoaderEntryOneShot` and `LoaderEntryDefault`, and `BootNext`. SeaBIOS
  (hcloud cx) has no EFI variables, none of these is a counter, and a target
  that panics cannot set them. So Limine always starts the bootstrap, and
  the bootstrap counts.
- **The counter is on the ESP**, not on the root file system. A mark on
  ext4 makes the bootstrap mount ext4 on each warm boot, with a journal
  replay after a reset. A counter that the target decrements in its own
  initrd misses a panic before that initrd. A direct `/target` entry after
  the first good boot makes warm boots cheaper, but a later panic or hang
  of that target then loops forever.
- **The ESP has an empty `/loader` directory.** systemd-stub keeps its
  random seed there. Without the directory, it logs "Failed to open random
  seed file", and each logged error stalls the boot for 2.5 s (`log_wait`
  in `src/boot/efi-log.c`).
- **zboot is for the UKI only.** On aarch64, `EFI_ZBOOT` makes the kernel
  an EFI application that decompresses itself. systemd-stub boots it.
  Limine's linux protocol does not: `common/protos/linux_risc.c` wants
  `ARM\x64` at offset 0x38, and zboot puts `LINUX_PE_MAGIC` there. arm64
  `kexec_file_load` does not either: `arch/arm64/kernel/kexec_image.c`
  takes only the Image magic. So the aarch64 release disk boots a UKI with
  the zboot kernel through a Limine `efi` entry, and the targets keep the
  Image.
- **x86_64 does not boot through the UKI.** The cx types boot SeaBIOS,
  which needs the linux entry anyway. On UEFI (cpx), a boot through the EFI
  stub of the x86 kernel, as a UKI or as an `efi` entry for the bzImage,
  was slower than Limine's linux protocol, which enters the kernel without
  the EFI stub. So the x86_64 release disk uses the linux entry on both
  firmwares.
- **mke2fs runs with `-E nodiscard`.** By default, mke2fs discards the
  whole new partition. That cost 1.3–1.8 s on cax. A disk made from a
  sparse snapshot gets nothing from the discard.
- **The platform kernels have no VFAT.** The bootstrap edits the ESP with
  the `fatfs` crate, and `boot-good.sh` uses mtools on the raw partition.
- **arm64 kexec needs `SUSPEND`.** CPU hotplug parks the secondary CPUs.
- **hcloud leases a /32.** The DHCP client adds a host route to the
  gateway.
- **The initrd is small.** spore has the Mozilla CA roots built in
  (`webpki-roots`), so the initrd has no CA bundle. The bundled SQLite has
  FTS3, FTS5, RTREE, DBSTAT, STAT4, JSON, column metadata and
  load_extension off: `db.rs` only inserts rows. `idna_adapter` 1.0.0 is the
  ASCII-only IDNA of the url crate, so a substituter host name must be
  ASCII or punycode. mke2fs is built without libarchive, with `-Os` and
  `--gc-sections`. busybox is in the debug initrd only, with `allnoconfig`
  plus the applets of the emergency shell.
- **The cx23 CPU (Skylake) has no SHA-NI**, so the fetch is CPU bound
  there.

## Measurements

Date: 2026-09-19. Location: hcloud fsn1. The code is the code of this
commit.

Method:

- Snapshots `spore-hcloud-x86_64-v1` and `spore-hcloud-aarch64-v1`, made
  from the release disks as in "Upload to hcloud".
- The targets are `packages.<arch>-linux.hcloud-target`. A signed HTTP
  cache in the same location serves them, with zstd NARs.
- One user-data file serves all types. Its `system` is a map with one path
  for each arch.
- Three creates per type, cx23, cpx22, cax11 and cax21. All 12 servers
  start at the same time.
- "API" is the time of `hcloud server create`. The other times start when
  that call returns. "Bootstrap ping" is the first ping reply. "Target
  running" means: ssh works and `/run/current-system` is the target.
- "Target boot" is the `systemd-analyze` total of the target on its first
  boot.
- "Warm reboot": on one server per type, 3 sysrq `b` resets. Each time goes
  from the reset to a root ssh login. Each boot goes through the bootstrap
  and its boot counter.
- The bootstrap phases come from `/var/log/spore.log`.

### Sizes

| Artifact | x86_64 | aarch64 |
| -------- | ------ | ------- |
| Kernel | 7.70 MB bzImage | 17.03 MB Image, 6.45 MB zboot |
| Initrd, release | 1.82 MB | 1.77 MB |
| Initrd, debug | 2.02 MB | 1.98 MB |
| UKI | 9.69 MB | 8.38 MB (zboot kernel) |
| `spore` binary | 3.15 MB | 2.77 MB |
| Release disk, raw | 131 MiB | 131 MiB |
| Release disk, zstd -19 | 9.70 MB | 8.38 MB |
| Debug disk, zstd -19 | 9.91 MB | 8.59 MB |
| hcloud snapshot | 0.0096 GB | 0.0083 GB |
| Lean target closure | 274.0 MiB, 287 paths | 332.6 MiB, 286 paths |
| Lean target kernel and initrd | 7.70 MB and 19.18 MB | 17.03 MB and 19.01 MB |

The release files are the zstd disks, the UKIs, the binaries and
`SHA256SUMS`: 60.6 MB for the two platforms.

### Create to target

| Type  | API | Bootstrap ping | Target running | API + target running | Target boot | Warm reboot to login |
| ----- | --- | -------------- | -------------- | -------------------- | ----------- | -------------------- |
| cx23  | 19.8 s | 0.0 s | 3.9–5.5 s | 23.7–25.3 s | 2.88–3.06 s | 5.20–6.39 s |
| cpx22 | 9.7–12.2 s | 5.5–6.8 s | 10.0–10.4 s | 19.8–22.2 s | 1.98–2.29 s | 9.34–9.68 s |
| cax11 | 8.0–9.7 s | 5.2–6.6 s | 10.3–11.5 s | 18.7–20.0 s | 2.56–2.65 s | 10.00–10.03 s |
| cax21 | 8.0–9.6 s | 5.5–6.6 s | 10.8–11.8 s | 19.6–20.4 s | 2.41–2.53 s | 10.39–10.49 s |

### The bootstrap on the first boot

The first two rows are from the reset and from the kernel start. The other
rows are from the start of /init.

| Step | cx23 | cpx22 | cax11 | cax21 |
| ---- | ---- | ----- | ----- | ----- |
| /init, after the reset | 5.24–5.48 s | 8.38–8.88 s | 7.42–7.50 s | 7.59–7.66 s |
| /init, after the kernel start | 0.79–0.98 s | 0.59–0.62 s | 0.26–0.31 s | 0.28–0.29 s |
| Network up | 0.02–0.31 s | 0.06–0.31 s | 0.05–0.33 s | 0.32–0.33 s |
| Disk ready | 0.11–0.38 s | 0.12–0.37 s | 0.12–0.40 s | 0.40 s |
| Closure fetched | 1.13–1.28 s | 0.74–0.93 s | 0.84–1.11 s | 0.89–0.91 s |
| Paths fetched | 287 | 287 | 286 | 286 |
| Boot entry written | 1.24–1.43 s | 0.81–1.02 s | 0.97–1.26 s | 1.00–1.02 s |
| kexec | 1.30–1.48 s | 0.84–1.05 s | 1.35–1.64 s | 1.74–1.78 s |
| Peak memory of /init | 53.6–66.1 MiB | 46.4–58.9 MiB | 49.9–52.8 MiB | 44.9–55.3 MiB |

In the disk step, mke2fs took 18–32 ms and the GPT re-read took 8–25 ms,
with no retries.

### Boot-count fallback

cx23, `boot-tries` 1, and the lean target with a health check that always
fails and a 30 s deadline. The target never marks a boot good. Times are
from the start of the create call, at the first ssh answer of each boot.

| Time    | Boot | `tries` | What happened |
| ------- | ---- | ------- | ------------- |
| 16.5 s  | 1 | 0 1 | First boot: provision, kexec 0.79 s after /init |
| 48.9 s  | 2 | 0 1 | softdog reset; "the target used all 1 tries without a good boot"; provision again, 0 paths, kexec 0.19 s after /init |
| 81.6 s  | 3 | 0 1 | The same, kexec 0.22 s after /init |
| 114.5 s | 4 | 0 1 | The same, kexec 0.23 s after /init |
| 146.2 s | 5 | 0 1 | The same, kexec 0.20 s after /init |

Each boot of the bad target lasts about 33 s: the 30 s deadline, the reset
and the boot. The loop does not stop, because the same user-data names the
same bad system each time. `"fallback": "rescue"` stops it in the
bootstrap. The peak memory of /init was 54.4 MiB on the first boot and
32.5–33.4 MiB on the boots that fetched nothing.

### spore pull

`spore pull` of the x86_64 lean target (287 paths, 274 MiB) on a Debian
cx23, from the same cache, 5 runs each. The time is the one that
`spore pull` logs. The memory is the maximum resident set from GNU time.

| Root  | Time | Peak memory |
| ----- | ---- | ----------- |
| tmpfs | 1.23–1.42 s | 61.8–72.2 MiB |
| ext4  | 1.54–1.72 s | 58.5–65.1 MiB |

### Notes

- On cx23, the create call returned only after the server was up: 19.8 s.
  So the bootstrap ping is 0.0 s, and "target running" is only the end of
  the boot. Use only "API + target running" to compare the types.
- On every type, the network step took 0.02–0.07 s on some servers and
  0.30–0.33 s on others. The cause is not known.
- On cax and cpx, most of the time to /init is the reset and the UEFI
  firmware.
- No unit failed on any target.

## Limits

- The bootstrap takes a repart layout, but ext4 only, and no encryption or
  RAID.
- Boot counting runs the bootstrap on every warm boot, which adds a kexec.
- The aarch64 kernel has no softdog yet, so a hang there waits for a reset
  from outside. Its kernel.config needs a native aarch64 build.
- The ESP entry is Limine specific. No boot specification support.
- The kexec payload is trusted through the substituter signature only.
