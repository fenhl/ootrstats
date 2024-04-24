setting up an ootrstats worker with `bench` support on a fresh Windows PC

1. Install `scoop` via the instructions at <https://scoop.sh/>.
2. Run `scoop install git python`.
3. Run `wsl --install` and reboot when done.
4. Run `wsl --update --pre-release` to get support for the CPU instruction counter.
5. Run `wsl sudo apt-get update && wsl sudo apt-get install build-essential libgtk-3-dev linux-tools-generic` to install:
    * `gcc` (`build-essential`) which is required by cargo
    * `libgtk-3-dev` which is required for [`rfd`](https://docs.rs/rfd)
    * `perf` (`linux-tools-generic`) which is used by the `bench` subcommand
6. Symlink `/usr/lib/linux-tools/*-generic/perf` into your WSL `PATH`.
7. [Install Rust](https://www.rust-lang.org/tools/install) inside WSL.
8. In Windows Settings, go to System → Optional features → Add an optional feature and enable the “OpenSSH Server” feature.
9. In the Services app, double-click on OpenSSH SSH Server, set Startup type to Automatic, click Start, then OK.
10. Connect once from the supervisor to verify the SSH host key. You can check the host key by running `sudo ssh-keygen -lf C:\ProgramData\ssh\ssh_host_ed25519_key.pub` on the worker, where `sudo` can be installed via `scoop install sudo`.
11. Allow `~\.cargo\bin\ootrstats-worker-daemon` through Windows Firewall.
12. Run `ootrstats-worker-daemon`.
