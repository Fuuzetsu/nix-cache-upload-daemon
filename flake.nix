{
  description = "nix-cache-upload-daemon";
  nixConfig.bash-prompt = "[nix-develop]$ ";

  inputs = {
    flake-utils.url = "github:numtide/flake-utils";
    nixpkgs.url = "github:NixOS/nixpkgs";
    # A version of nixpkgs where numpy is old so scikit-optimize still works...
    nixpkgs_scikit.url = "github:NixOS/nixpkgs/86ddda113be74c1f408d05afaad5012c78cd987a";
    # Need recent rust-overlay for recent nixpkgs:
    # https://github.com/oxalica/rust-overlay/issues/121
    rust-overlay = {
      url = "github:oxalica/rust-overlay/master";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-utils.follows = "flake-utils";
    };
    flake-compat = {
      url = "github:edolstra/flake-compat";
      flake = false;
    };
    nixGL = {
      url = "github:guibou/nixGL";
      flake = false;
    };

    crate2nix = {
      url = "github:kolloch/crate2nix";
      flake = false;
    };
  };

  outputs =
    inputs@{ self
    , nixpkgs
    , rust-overlay
    , flake-utils
    , flake-compat
    , nixGL
    , crate2nix
    , nixpkgs_scikit
    }:
    flake-utils.lib.eachDefaultSystem (system:
    let


      # Vanilla nixpkgs with some minor overrides for tooling. This is useful
      # for using programs that we don't care to do rust overrides for. For
      # example, we override rustc versions in `pkgs`. This means that if we
      # care about some program that uses rust, we will end up building it
      # ourselves. In most cases, we don't really care though: we just want
      # the binary. Therefore, for any packages that don't want to do
      # overrides for, we just use this.
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ ];
      };


      rust = import ./nix/rust.nix {
        inherit system rust-overlay;
        inherit (inputs.self) sourceInfo;
        vanillaPackages = pkgs;
      };
      crate2nix = rust.rustToolchainPkgs.rustPlatform.buildRustPackage
        {
          pname = (pkgs.lib.importTOML "${inputs.crate2nix}/crate2nix/Cargo.toml").package.name;
          version = (pkgs.lib.importTOML "${inputs.crate2nix}/crate2nix/Cargo.toml").package.version;
          src = "${inputs.crate2nix}/crate2nix";
          doCheck = false;
          cargoLock = {
            lockFile = "${inputs.crate2nix}/crate2nix/Cargo.lock";
          };
          nativeBuildInputs = [ pkgs.makeWrapper ];
          patches = [ ./nix/crate2nix-sort-dependencies.patch ];
          postFixup = ''
            wrapProgram $out/bin/crate2nix \
                --prefix PATH : ${pkgs.lib.makeBinPath [ rust.rustToolchainPkgs.cargo pkgs.nix pkgs.nix-prefetch-git ]}
          '';
        };
      minNixVersion = "2.5";
    in
    assert pkgs.lib.asserts.assertMsg (! pkgs.lib.versionOlder builtins.nixVersion minNixVersion)
      "Minimum supported nix version for engine is ${minNixVersion} but trying to run with ${builtins.nixVersion}. Ask in #nix Slack channel if you need help upgrading.";
    {
      legacyPackages = pkgs;
      packages = flake-utils.lib.flattenTree
        (rust.workspaceCrates
          // {
          inherit crate2nix;
          # A whole set of crates, useful for building every crate in workspace in
          # CI and such.
          workspace-crates = pkgs.linkFarm "workspace-crates"
            (pkgs.lib.attrValues
              (pkgs.lib.mapAttrs
                (name: path: { inherit name path; })
                rust.workspaceCrates));
        }
        );

      devShell = pkgs.mkShell {
        nativeBuildInputs = [
          rust.nativeBuildInputs

        ];
        buildInputs = [
          rust.buildInputs
          # cargo, rustc
          rust.rustToolchainPkgs.rustc

          # bin/git-version.sh
          pkgs.git
        ];
        # Required by test-suite and in general let's set a uniform one.
        LANG = "C.UTF-8";

        # We set RUSTUP_HOME to non-standard location to avoid mixing
        # nix-provided rustup tools with those that user may have from some
        # other source. This is necessary as the nix tools tend to have a
        # recent glibc while other source use some old one and mixing the two
        # ends up poorly.
        #
        # https://tsuru.slack.com/archives/CDW0NFX96/p1617763514001500
        shellHook = ''
          export RUSTUP_HOME=$HOME/.rustup-nix-cache-upload-daemon
        '';
      };
    });
}
