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
                pname = "ootrstats-worker-daemon";
                version = manifest.version;
                src = ./.;
                cargoBuildFlags = "--package=ootrstats-worker-daemon";
                cargoLock = {
                    lockFile = ./Cargo.lock;
                    outputHashes = {
                        "decompress-3.0.1" = "sha256-g1B6DcUcs5bhMGuBwSy/V1jg6Cwkc5MmLi9Vvl5naKg=";
                        "log-lock-0.2.5" = "sha256-YuS4YzhVDI6kclnad4LCaiUf2/jPesI9ECaR+cS8Ua0=";
                        "ootr-utils-0.6.3" = "sha256-RzqUlguQmYYkiHs/5UKf3eQEcIh4k5mVGGwRwqjqbso=";
                        "rocket-util-0.2.15" = "sha256-j/kxIwbiBlzWIRl0CfGep1GiRvbq8MfeZse0CxntD/E=";
                        "wheel-0.15.0" = "sha256-nNn6Rwnc/6ZLzJdgR2to2xPpgo2mKPMs6dtDHzIbPMc=";
                    };
                };
            };
        });
    };
}