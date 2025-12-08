{
    inputs = {
        # a better way of using the latest stable version of nixpkgs
        # without specifying specific release
        nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/*.tar.gz";
    };
    outputs = { self, nixpkgs }: let
        supportedSystems = [
            "aarch64-darwin"
            "aarch64-linux"
            "x86_64-darwin"
            "x86_64-linux"
        ];
        forEachSupportedSystem = f: nixpkgs.lib.genAttrs supportedSystems (system: f {
            pkgs = import nixpkgs { inherit system; };
        });
    in {
        packages = forEachSupportedSystem ({ pkgs, ... }: let
            manifest = (pkgs.lib.importTOML ./Cargo.toml).workspace.package;
        in {
            default = pkgs.rustPlatform.buildRustPackage {
                buildAndTestSubdir = "crate/ootrstats-worker-daemon";
                buildFeatures = [
                    "nixos"
                ];
                cargoLock = {
                    allowBuiltinFetchGit = true; # allows omitting cargoLock.outputHashes
                    lockFile = ./Cargo.lock;
                };
                pname = "ootrstats-worker-daemon";
                src = ./.;
                version = manifest.version;
            };
        });
    };
}