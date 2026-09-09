{
  description = "logos-tx-decoder — offline EVM calldata decoding, as a linkable library.";

  inputs = {
    # Only for its nixpkgs: the static archive is linked into Logos Qt plugins,
    # so it must be built against the same libc and toolchain they are.
    logos-module-builder.url = "github:logos-co/logos-module-builder";
  };

  outputs = inputs@{ self, logos-module-builder, ... }:
    let
      nixpkgs = logos-module-builder.inputs.nixpkgs;
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems f;

      # The archive is linked into Qt plugins, so it has to reach every target
      # they do. x86_64-windows is the builder's cross pseudo-system, and
      # common.mkPkgs is what routes it to logos-nix's mingw package set --
      # nixpkgs.legacyPackages has no such attribute.
      targets = systems ++ [ "x86_64-windows" ];
      forAllTargets = f: nixpkgs.lib.genAttrs targets f;

      mkLib = system:
        let pkgs = logos-module-builder.lib.common.mkPkgs system;
        in pkgs.rustPlatform.buildRustPackage {
          pname = "logos-tx-decoder";
          version = "1.0.0";
          # Never copy a local `target/` into the store: it is hundreds of MB
          # of build cache and nix path: sources do not honour .gitignore.
          src = pkgs.lib.cleanSourceWith {
            src = ./rust-lib;
            filter = path: _type: baseNameOf path != "target";
          };
          cargoLock.lockFile = ./rust-lib/Cargo.lock;

          # The pure cores plus the C ABI. Cheap, and the whole point of the
          # library is that it is correct without a runtime to host it.
          # Under cross the test binary is a PE the Linux builder cannot run.
          doCheck = pkgs.stdenv.hostPlatform == pkgs.stdenv.buildPlatform;

          # A staticlib crate installs nothing by default. Ship the archive and
          # the header in the lib/+include/ layout LogosModule.cmake resolves.
          installPhase = ''
            runHook preInstall
            mkdir -p $out/lib $out/include
            find target -name 'liblogos_tx_decoder.a' -exec cp {} $out/lib/ \;
            cp ${./include/tx_decoder.h} $out/include/tx_decoder.h
            test -f $out/lib/liblogos_tx_decoder.a
            runHook postInstall
          '';
        };
    in
    {
      packages = forAllTargets (system: rec {
        logos_tx_decoder = mkLib system;
        default = logos_tx_decoder;
      });

      checks = forAllSystems (system: {
        tests = mkLib system;
      });
    };
}
