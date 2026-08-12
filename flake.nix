{
    inputs.flake.url = "github:fenhl/flake";
    outputs = attrs: attrs.flake.lib {
        nixosConfigurations = {
            bootstrap = { lib, ... }: lib.nixosSystem {
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
            ootrstats = { lib, ... }: lib.nixosSystem {
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
        packages = {
            default = { pkgs, ... }: let
                manifest = (pkgs.lib.importTOML ./Cargo.toml).workspace.package;
            in pkgs.rustPlatform.buildRustPackage {
                inherit (manifest) version;
                pname = "ootrstats";
                buildAndTestSubdir = "crate/ootrstats-supervisor";
                buildFeatures = [
                    "nixos"
                ];
                cargoLock = {
                    allowBuiltinFetchGit = true; # allows omitting cargoLock.outputHashes
                    lockFile = ./Cargo.lock;
                };
                nativeBuildInputs = with pkgs; [
                    installShellFiles # required for `installShellCompletion` in postInstall hook
                    makeWrapper # required for wrapProgram in postFixup hook
                ];
                postFixup = ''
                    wrapProgram $out/bin/ootrstats --prefix PATH : ${pkgs.lib.makeBinPath (with pkgs; [
                        cargo # required to build OoTR riir branch
                        clang # required to fix the error “linker `cc` not found” while building OoTR riir branch
                        git #TODO replace usage of the git CLI in ootrstats with gix
                        (python3.withPackages (python-pkgs: [
                            python-pkgs.requests # required for the RSL script
                        ]))
                    ] ++ pkgs.lib.optional stdenv.hostPlatform.isLinux perf)}
                '';
                postInstall = let
                    ootrstats = "${pkgs.stdenv.hostPlatform.emulator pkgs.buildPackages} $out/bin/ootrstats";
                in pkgs.lib.optionalString (pkgs.stdenv.hostPlatform.emulatorAvailable pkgs.buildPackages) ''
                    installShellCompletion --cmd ootrstats \
                        --bash <(COMPLETE=bash ${ootrstats}) \
                        --fish <(COMPLETE=fish ${ootrstats}) \
                        --zsh <(COMPLETE=zsh ${ootrstats})
                '';
                src = ./.;
            };
            worker-daemon = { pkgs, ... }: let
                manifest = (pkgs.lib.importTOML ./Cargo.toml).workspace.package;
            in pkgs.rustPlatform.buildRustPackage {
                inherit (manifest) version;
                pname = "ootrstats-worker-daemon";
                buildAndTestSubdir = "crate/ootrstats-worker-daemon";
                buildFeatures = [
                    "nixos"
                ];
                cargoLock = {
                    allowBuiltinFetchGit = true; # allows omitting cargoLock.outputHashes
                    lockFile = ./Cargo.lock;
                };
                nativeBuildInputs = with pkgs; [
                    makeWrapper # required for wrapProgram in postFixup hook
                ];
                postFixup = ''
                    wrapProgram $out/bin/ootrstats-worker-daemon --prefix PATH : ${pkgs.lib.makeBinPath (with pkgs; [
                        cargo # required to build OoTR riir branch
                        clang # required to fix the error “linker `cc` not found” while building OoTR riir branch
                        git #TODO replace usage of the git CLI in ootrstats with gix
                        (python3.withPackages (python-pkgs: [
                            python-pkgs.requests # required for the RSL script
                        ]))
                    ] ++ pkgs.lib.optional stdenv.hostPlatform.isLinux perf)}
                '';
                src = ./.;
            };
        };
    };
}
