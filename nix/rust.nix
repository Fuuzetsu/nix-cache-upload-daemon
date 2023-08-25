{ system, vanillaPackages, rust-overlay, sourceInfo }:

let
  rustToolchain = pkgs:
    let
      rustChannel = (pkgs.rust-bin.fromRustupToolchainFile ../rust-toolchain).override {
        extensions = [
          "clippy"
          "rust-analysis"
          "rust-docs"
          "rust-src"
          "rustfmt"
        ];
      };
    in
    {
      rustc = rustChannel;
      cargo = rustChannel;
      rust-fmt = rustChannel;
      rust-std = rustChannel;
      clippy = rustChannel;
      rustPlatform = pkgs.makeRustPlatform {
        rustc = rustChannel;
        cargo = rustChannel;
      };
    };


  # Set of packages where all Rust tools come from the rustToolchain, determined
  # by the rust-toolchain file.
  rustToolchainPkgs = import (vanillaPackages.path) {
    inherit system;
    overlays = [
      (import rust-overlay)
      (self: _: rustToolchain self)
    ];
  };

  # This is a set of all the extra system dependencies that any
  # rust crates we depend on need. We can stick this into the nix
  # shell environment and we should have the same set whether we
  # build via nix or via cargo in a shell.
  crateBuildTimeOverrides = import ./rust_build_overrides.nix rustToolchainPkgs;

  # We now import all our crate definitions, including our workspace crates.
  # Notice that we use the right set of packages (derived from rust-toolchain).
  cargoNix = import ./Cargo.nix {
    pkgs = rustToolchainPkgs;
    buildRustCrateForPkgs = pkgs:
      pkgs.buildRustCrate.override {
        # Note that normally one would start with pkgs.defaultCrateOverrides
        # here and then override that further with own set. Instead, we start
        # with our own set! Why? Because we explicitly traverse over this set to
        # extract the dependencies for use in nix-shell: therefore we'll
        # explicitly add things to rust_build_overrides.nix even if they already
        # have a good default in nixpkgs as that basically indicates _which_ crates we care about.
        defaultCrateOverrides = crateBuildTimeOverrides.defaultCrateOverrides;
      };
  };

  # Build derivations for all our workspaces
  workspaceMembers = vanillaPackages.lib.mapAttrsToList
    (_: crate: crate.build)
    (cargoNix.workspaceMembers);

  # Information about any extra runtime tools needed by the crates.
  #
  # We use vanillaPackages for the runtime dependencies: usually for regular
  # tools, we don't really care how what rust they were built with. For example,
  # `skopeo` has a rust dependency in its build chain but we don't want to build
  # our own version as we change the rust-toolchain. If we do need specific
  # tools built with specific rust, we can pass them explicitly.
  crateRunTime = import ./rust_runtime_dependencies.nix { pkgs = vanillaPackages; inherit sourceInfo; };

  # Given a single crate, create a wrapper with runtime dependencies if
  # necessary.
  workspaceCrates = builtins.listToAttrs
    (builtins.map
      (raw_crate:
        let
          # Wrap a crate with any runtime inputs, if any at all.
          wrappedCrate = crateRunTime.ensureRuntimeInputs raw_crate;
          metaCrate = wrappedCrate.overrideAttrs (_: {
            # We set meta.mainProgram to the crate name. This allows nix run to
            # just work like `nix run .#isim <feedspec>` for flakes and
            # presumably some similar form for vanilla form.
            meta.mainProgram = raw_crate.crateName;
          });
          crate = metaCrate
            # We carefully preserve `crateName` for any other uses of the attribute
            # such as when making test derivations. We have to set it after
            # overrideAttrs as that will discard it.
            // {
            crateName = raw_crate.crateName;
            packageId = cargoNix.workspaceMembers.${raw_crate.crateName}.packageId;
          };
        in
        vanillaPackages.lib.nameValuePair raw_crate.crateName crate
      )
      workspaceMembers);

in
{
  inherit rustToolchainPkgs workspaceCrates;
  inherit (crateBuildTimeOverrides) nativeBuildInputs;
  # Any extra build time and run time dependencies we declared for any crates. Putting this
  # in the shell environment should ensure we're not missing any system
  # libraries during regular cargo builds in the shell.
  buildInputs = crateBuildTimeOverrides.buildInputs ++ builtins.attrValues crateRunTime.runTimeDependencies;

  runTimeVariables = vanillaPackages.lib.foldr (l: r: l // r) { } (builtins.attrValues crateRunTime.runTimeVariables);
}
