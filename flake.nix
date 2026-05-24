{
    inputs.nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/*.tar.gz";
    outputs = attrs: let
        supportedSystems = [
            "aarch64-darwin"
            "aarch64-linux"
            "x86_64-darwin"
            "x86_64-linux"
        ];
        forEachSupportedSystem = f: attrs.nixpkgs.lib.genAttrs supportedSystems (system: f {
            pkgs = import attrs.nixpkgs {
                inherit system;
            };
        });
    in {
        nixosConfigurations = {
            bootstrap = attrs.nixpkgs.lib.nixosSystem {
                modules = [
                    ({ modulesPath, pkgs, ... }: {
                        environment = {
                            loginShellInit = ''
                                # automatically switch to the full config on first boot
                                [[ "$(tty)" == /dev/ttyS0 ]] \
                                    && nixos-rebuild switch --recreate-lock-file --refresh --no-write-lock-file --flake=github:fenhl/ootrstats \
                                    && ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub
                            '';
                            systemPackages = with pkgs; [
                                git # required to switch to the ootrstats system config
                            ];
                        };
                        imports = [
                            "${modulesPath}/virtualisation/linode-config.nix"
                        ];
                        networking.hostName = "ootrstats";
                        nixpkgs.hostPlatform = "x86_64-linux";
                        services.getty.autologinUser = "root"; # automatically log in on startup to continue the bootstrap sequence
                        system.stateVersion = "25.11"; # should NEVER be changed, see Nix option description
                    })
                ];
                specialArgs = attrs;
            };
            ootrstats = attrs.nixpkgs.lib.nixosSystem {
                modules = [
                    ({ modulesPath, pkgs, ... }: {
                        environment.systemPackages = [
                            attrs.self.packages.${pkgs.stdenv.hostPlatform.system}.worker-daemon
                        ];
                        imports = [
                            "${modulesPath}/virtualisation/linode-config.nix"
                        ];
                        networking.hostName = "ootrstats";
                        nixpkgs.hostPlatform = "x86_64-linux";
                        system.stateVersion = "25.11"; # should NEVER be changed, see Nix option description
                    })
                ];
                specialArgs = attrs;
            };
        };
        packages = forEachSupportedSystem ({ pkgs, ... }: let
            manifest = (pkgs.lib.importTOML ./Cargo.toml).workspace.package;
        in rec {
            default = supervisor;
            supervisor = pkgs.rustPlatform.buildRustPackage {
                buildAndTestSubdir = "crate/ootrstats-supervisor";
                buildFeatures = [
                    "nixos"
                ];
                cargoLock = {
                    allowBuiltinFetchGit = true; # allows omitting cargoLock.outputHashes
                    lockFile = ./Cargo.lock;
                };
                pname = "ootrstats";
                src = ./.;
                version = manifest.version;
            };
            worker-daemon = pkgs.rustPlatform.buildRustPackage {
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