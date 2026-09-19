{
  description = "spore: one small boot image that becomes any Nix closure";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/941ede61f9ab033fbf284f859af3b7f92562b85f";
  inputs.hercules-ci-effects = {
    url = "github:hercules-ci/hercules-ci-effects";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    { nixpkgs, hercules-ci-effects, ... }:
    let
      inherit (nixpkgs) lib;
      # platforms/<name>-<arch>: outputs go to packages.<arch>-linux.<name>-*
      platforms = lib.mapAttrsToList (
        dir: _:
        let
          m = builtins.match "(.*)-([^-]+)" dir;
          system = "${lib.elemAt m 1}-linux";
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          name = lib.elemAt m 0;
          arch = lib.elemAt m 1;
          inherit dir system pkgs;
          value = import ./platforms/${dir} {
            inherit pkgs;
            common = import ./common { inherit pkgs; };
          };
        }
      ) (lib.filterAttrs (_: t: t == "directory") (builtins.readDir ./platforms));
      perPlatform =
        f:
        lib.foldl' lib.recursiveUpdate { } (
          map (p: { ${p.system} = lib.mapAttrs' (n: lib.nameValuePair "${p.name}-${n}") (f p); }) platforms
        );
      x86 = nixpkgs.legacyPackages.x86_64-linux;
      version = lib.fileContents ./VERSION;
      release = import ./release.nix {
        pkgs = x86;
        inherit version;
        platforms = map (p: p // { binary = (import ./common { inherit (p) pkgs; }).spore; }) platforms;
      };
    in
    {
      packages =
        lib.recursiveUpdate
          (perPlatform (p: {
            inherit (p.value) kernel initrd disk;
            kernel-config = p.value.kernelConfig;
            initrd-rust = p.value.rustInitrd;
            disk-rust = p.value.rustDisk;
            disk-uki = p.value.ukiDisk;
            disk-debug = p.value.debugDisk;
            initrd-debug = p.value.debugInitrd;
            inherit (p.value.ukiDisk) uki;
            inherit (p.value) target;
          }))
          {
            x86_64-linux = {
              spore = (import ./common { pkgs = x86; }).spore;
              inherit release;
              # built on x86_64 while no arm builder runs
              spore-aarch64 = (import ./common { pkgs = x86.pkgsCross.aarch64-multiplatform-musl; }).spore;
            };
          };

      checks = perPlatform (
        p:
        lib.mapAttrs' (n: lib.nameValuePair "boot-${n}") (
          import ./common/test.nix (
            {
              inherit (p) pkgs;
              platform = p.dir;
              inherit (p.value) disk;
            }
            // p.value.test
          )
        )
        // lib.mapAttrs' (n: lib.nameValuePair "boot-rust-${n}") (
          import ./common/test.nix (
            {
              inherit (p) pkgs;
              platform = "${p.dir}-rust";
              disk = p.value.rustDisk;
              rust = true;
            }
            // p.value.test
          )
        )
        # the rescue lane per firmware, on the disk with the shell
        // lib.mapAttrs' (n: lib.nameValuePair "boot-debug-${n}") (
          lib.filterAttrs (n: _: lib.hasSuffix "-rescue" n) (
            import ./common/test.nix (
              {
                inherit (p) pkgs;
                platform = "${p.dir}-debug";
                disk = p.value.debugDisk;
                rust = true;
                shell = true;
              }
              // p.value.test
            )
          )
        )
        # the good lane per firmware: every boot goes through the UKI
        // lib.mapAttrs' (n: lib.nameValuePair "boot-uki-${n}") (
          lib.getAttrs (lib.attrNames p.value.test.firmwares) (
            import ./common/test.nix (
              {
                inherit (p) pkgs;
                platform = "${p.dir}-uki";
                disk = p.value.ukiDisk;
                rust = true;
              }
              // p.value.test
            )
          )
        )
        // {
          kernel-config = p.pkgs.runCommand "${p.dir}-kernel-config-in-sync" { } ''
            diff -u ${./platforms/${p.dir}/kernel.config} ${p.value.kernelConfig} || {
              echo "regenerate: nix build .#${p.name}-kernel-config and copy it to platforms/${p.dir}/kernel.config" >&2
              exit 1
            }
            touch $out
          '';
        }
      );

      # nixbot builds onPush.default.outputs on every push and runs the
      # effects on main only
      herculesCI =
        { primaryRepo, ... }:
        {
          onPush.default.outputs = {
            inherit release;
            effects.release = import ./release-effect.nix {
              pkgs = x86;
              hci-effects = hercules-ci-effects.lib.withPkgs x86;
              inherit version release;
              inherit (primaryRepo) rev;
              repo = "numtide/spore";
            };
          };
        };
      devShells.x86_64-linux.default = x86.mkShellNoCC {
        packages = [
          x86.jq
          x86.mtools
          x86.qemu_kvm
          x86.util-linux
        ];
      };

      formatter.x86_64-linux = x86.writeShellApplication {
        name = "fmt";
        runtimeInputs = [
          x86.nixfmt-rfc-style
          x86.shfmt
          x86.rustfmt
        ];
        text = ''
          git ls-files -z '*.nix' | xargs -0 nixfmt
          git ls-files -z common/init common/dhcp-script 'platforms/*.sh' 'targets/*.sh' | xargs -0 shfmt -w -i 2
          rustfmt --edition 2024 common/spore/src/main.rs
        '';
      };
    };
}
