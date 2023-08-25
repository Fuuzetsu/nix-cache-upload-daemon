let
  runtimeInputs = { };
in
{ pkgs, sourceInfo }:

rec {
  # A function that given a crate, checks if we have to the binary in a script
  # that allows it to find some programs at runtime and does it if so. Passes
  # create through untouched if wrapping is unnecessary.
  ensureRuntimeInputs = crate: pkgs.symlinkJoin
    {
      inherit (crate) name;
      paths = [ crate ];
      buildInputs = [ pkgs.makeWrapper ];
      # We assume that the crate name is the same as the binary name. If there
      # are more binaries or they have different names, we can extend this
      # function to deal with that.
      postBuild =
        let
          inputs =
            if ! runtimeInputs ? "${crate.crateName}"
            then [ ]
            else
              pkgs.lib.mapAttrsToList
                (pkg: _: runTimeDependencies."${pkg}")
                (builtins.functionArgs runtimeInputs."${crate.crateName}");
          variables =
            if ! runTimeVariables ? "${crate.crateName}"
            then [ ]
            else
              pkgs.lib.mapAttrsToList
                (variable: value: "--set ${variable} ${value}")
                runTimeVariables."${crate.crateName}";
          pathPrefix =
            if inputs != [ ]
            then "--prefix PATH : ${pkgs.lib.makeBinPath inputs}"
            else "";
          surfaceModelRevision =
            if sourceInfo ? rev
            then "--set SURFACE_MODEL_COMMIT ${sourceInfo.rev}"
            # If repo is dirty and we built via nix, we don't want SURFACE_MODEL_COMMIT
            # to be set. Ensure it's not set externally.
            else "--unset SURFACE_MODEL_COMMIT";
          # Taken from https://github.com/NixOS/nix/blob/130284b8508dad3c70e8160b15f3d62042fc730a/src/libutil/hash.cc#L84
          nixHashChars = "0123456789abcdfghijklmnpqrsvwxyz";
          # Extracts only the hash part of a nix store path.
          sourceHash = builtins.head (builtins.match "${builtins.storeDir}/([${nixHashChars}]+)-.*" sourceInfo.outPath);
        in
        ''
          for prg in "$out"/bin/*; do
            wrapProgram "$prg" \
              --set SURFACE_MODEL_NIX_SOURCE_HASH ${sourceHash} \
              ${pkgs.lib.concatStringsSep " " variables} \
              ${surfaceModelRevision} \
              ${pathPrefix}
          done
        '';
    };

  runTimeVariables = { };

  # A set of runtime dependencies used by the crates. This is useful to put
  # inside nix shell environment: this will result in the dependencies being
  # present even if the user built the binary with cargo and therefore will be
  # running unwrapped binary.
  runTimeDependencies =
    # Similar to builtins.intersectAttrs but requires every input to be present
    # in the source set.
    let getPkgs = args: builtins.mapAttrs (n: _: pkgs."${n}") args;
    in
    getPkgs
      (pkgs.lib.foldr (args: xs: args // xs) { }
        (builtins.map (builtins.functionArgs)
          (builtins.attrValues runtimeInputs)));

}
