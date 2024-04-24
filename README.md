`ootrstats` is a tool for generating various statistics on the [Ocarina of Time randomizer](https://github.com/OoTRandomizer/OoT-Randomizer) by generating a large number of seeds and analyzing them.

# Installation

1. Install Rust:
    * On Windows, download and run [rustup-init.exe](https://win.rustup.rs/) and follow its instructions. If asked to install Visual C++ prerequisites, use the “Quick install via the Visual Studio Community installer” option. You can uncheck the option to launch Visual Studio when done.
    * On other platforms, please see [the Rust website](https://www.rust-lang.org/tools/install) for instructions.
2. Open a command line:
    * On Windows, right-click the start button, then click “Terminal”, “Windows PowerShell”, or “Command Prompt”.
    * On other platforms, look for an app named “Terminal” or similar.
3. In the command line, run the following command. Depending on your computer, this may take a while; you can [configure ootrstats](#configuration) while it's running.

    ```
    cargo install --git=https://github.com/fenhl/ootrstats --branch=main ootrstats-supervisor
    ```

# Configuration

`ootrstats` requires a configuration file, which should be a [JSON](https://json.org/) object located at `$XDG_CONFIG_DIRS/ootrstats.json` on Unix, or `%APPDATA%\Fenhl\ootrstats\config\config.json` on Windows. It takes the following required entry:

* `workers`: An array of [worker configurations](#workers). You should specify at least one worker so seeds can be rolled.

And the following optional entry:

* `statsDir`: A path to a directory where the statistics will be stored. Defaults to `$XDG_DATA_DIRS/ootrstats` on Unix, or `%APPDATA%\Fenhl\ootrstats\data` on Windows.

## Workers

Each worker configuration is a JSON object with the following required entries:

* `name`: A string which will be displayed on the progress display on the command line, as well as in error messages.
* `kind`: One of the section headers listed below.

And the following optional entry:

* `bench`: If `false`, this worker is skipped when the `bench` subcommand is used. The default is `true`.

Depending on the `kind`, there are additional entries:

### `local`

A worker that runs on the same computer as the supervisor program. It takes the following additional required entry:

* `baseRomPath`: An absolute path to the vanilla OoT rom. See [the randomizer's documentation](https://github.com/OoTRandomizer/OoT-Randomizer#installation) for details.

And the following optional entries:

* `wslBaseRomPath`: An absolute path to the vanilla OoT rom that will be used if the randomizer is run inside [WSL](https://learn.microsoft.com/windows/wsl/about), i.e. for the `bench` subcommand if this worker is running on Windows. Defaults to `baseRomPath` if not specified.
* `cores`: The maximum number of instances of the randomizer to run in parallel. If this number is 0 or negative, it will be added to the number of available CPU cores, e.g. if 6 cores are detected and a number of `-2` is given, 4 cores will be used. Defaults to `-1`.

### `webSocket`

A worker that listens to WebSocket connections from the supervisor. To set up, do the following on the worker computer:

1. Install Rust
2. Run `cargo install --git=https://github.com/fenhl/ootrstats --branch=main ootrstats-worker-daemon`
    * If the worker computer is a Raspberry Pi, adding `--features=videocore-gencmd` is recommended. It makes the worker wait to start generating new seeds while the CPU temperature is above 80°C.
3. Create a JSON file at `$XDG_CONFIG_DIRS/ootrstats-worker-daemon.json` on Unix or `%APPDATA%\Fenhl\ootrstats\config\worker-daemon.json` on Windows, containing a JSON object with the following entries:
    * `password` (required): A password string that the supervisor will use to connect to this worker.
    * `address` (optional): The IP address on which the worker daemon will listen. Defaults to `127.0.0.1`, meaning only local connections will be accepted and you will need a reverse proxy like nginx. Change to `0.0.0.0` to accept connections from anywhere.
4. Start the worker daemon, e.g. by editing `assets/ootrstats-worker.service` inside a clone of this repository to adjust the username, then running `sudo systemctl enable --now assets/ootrstats-worker.service` if the worker is on a Linux distro that uses systemd.
5. Make the worker daemon reachable from the network, e.g. by enabling (an edited copy of) `assets/ootrstats.fenhl.net.nginx` if nginx is installed on the worker or by configuring the `address`.

The worker configuration on the supervisor takes the following additional required entries:

* `hostname`: The hostname or IP address of the worker, optionally with the port after a `:` separator (port defaults to 443 if `tls` is `true`, or to 80 otherwise).
* `password`: The password from step 3 of the worker setup described above.
* `baseRomPath`: An absolute path to the vanilla OoT rom on the worker computer. See [the randomizer's documentation](https://github.com/OoTRandomizer/OoT-Randomizer#installation) for details.

And the following optional entries:

* `tls`: Whether to use a secure WebSocket connection. If enabled, the worker needs a TLS certificate. The default is `true`.
* `wslBaseRomPath`: An absolute path to the vanilla OoT rom that will be used if the randomizer is run inside [WSL](https://learn.microsoft.com/windows/wsl/about), i.e. for the `bench` subcommand if this worker is running on Windows. Defaults to `baseRomPath` if not specified.
* `priorityUsers`: A list of usernames. The worker will not start rolling any new seeds while any of these users are signed in. Only supported by workers running on Windows.

# Usage

Run the `ootrstats-supervisor` command, followed by any options you would like to change from their defaults, followed by an optional subcommand. If no subcommand is given, the supervisor will simply generate the spoiler/error logs and place them in the [`statsDir`](#configuration).

The supervisor can be interrupted cleanly using <kbd>Ctrl</kbd><kbd>C</kbd> or <kbd>Ctrl</kbd><kbd>D</kbd>. If this is used, the supervisor will wait for seeds currently being rolled to finish before exiting, but will no longer start rolling any new seeds.

## Options

* `-b`, `--branch`: Specifies the git branch of the randomizer (or of the random settings script if combined with `--rsl`) to clone. Defaults to the repository's default branch.
* `-n`, `--num-seeds`: Specifies the sample size, i.e. how many seeds to roll. Any existing seeds will be reused. Defaults to 16384.
* `-p`, `--preset`: The name of the settings preset to use. Defaults to the Default/Beginner preset. Cannot be combined with `--rsl`.
* `-u`, `--github-user`: Specifies the GitHub user or organization name from which to clone the randomizer (or the random settings script if combined with `--rsl`). Defaults to `OoTRandomizer` (or `matthewkirby` if combined with `--rsl`).
* `--rev`: Specifies the git revision of the randomizer (or of the random settings script if combined with `--rsl`) to clone. Must be given as an unabbreviated git commit hash. Cannot be combined with `--branch`.
* `--rsl`: Roll seeds using [the random settings script](https://github.com/matthewkirby/plando-random-settings).
* `--settings`: The settings string to use for the randomizer. Cannot be combined with `--preset` or `--rsl`.
* `--suite`: Runs the benchmarking suite. Should usually be combined with the `bench` subcommand. Cannot be combined with `--preset` or `--rsl`. The benchmarking suite consists of:
    * the Default/Beginner preset
    * the current main tournament settings
    * the current multiworld tournament settings
    * Hell Mode
    * a version of the random settings script adjusted for compatibility with main Dev

## Subcommands

### `bench`

Benchmarks the randomizer by measuring the average number of CPU instructions required to successfully generate a seed, taking into account the failure rate of the randomizer.

This subcommand requires workers to have access to [`perf`](https://perf.wiki.kernel.org/), which is only available for Linux. Workers running on Windows will attempt to use [WSL](https://learn.microsoft.com/windows/wsl/about). To install `perf` on an Ubuntu or Debian distro running inside WSL, run `apt-get install linux-tools-generic` and copy/symlink `/usr/lib/linux-tools/*-generic/perf` into your `PATH`.

Results will be displayed on stdout.

If this subcommand is run with the `--raw-data` option, it will output the following data instead of displaying a summary. Each seed's data is printed on a separate line, starting with the character `s` for success or `f` for failure, followed by a space, followed by the number of instructions taken.

### `midos-house`

Collects statistics about the chest appearances in Mido's house, and saves them as a JSON file to the given path (a required positional argument). Used for generating the [midos.house](https://github.com/midoshouse/midos.house) logo.
