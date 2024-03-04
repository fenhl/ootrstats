`ootrstats` is a tool for generating various statistics on the [Ocarina of Time randomizer](https://github.com/OoTRandomizer/OoT-Randomizer) by generating a large number of seeds and analyzing them.

# Installation

1. (Skip this step if you're not on Windows.) If you're on Windows, you'll first need to download and install [Visual Studio](https://visualstudio.microsoft.com/vs/) (the Community edition should work). On the “Workloads” screen of the installer, make sure “Desktop development with C++” is selected. (Note that [Visual Studio Code](https://code.visualstudio.com/) is not the same thing as Visual Studio. You need VS, not VS Code.)
2. Install Rust:
    * On Windows, download and run [rustup-init.exe](https://win.rustup.rs/) and follow its instructions.
    * On other platforms, please see [the Rust website](https://www.rust-lang.org/tools/install) for instructions.
3. Open a command line:
    * On Windows, right-click the start button, then click “Terminal”, “Windows PowerShell”, or “Command Prompt”.
    * On other platforms, look for an app named “Terminal” or similar.
4. In the command line, run the following command. Depending on your computer, this may take a while; you can [configure ootrstats](#configuration) while it's running.

    ```
    cargo install --git=https://github.com/fenhl/ootrstats --branch=main ootrstats-supervisor
    ```

# Configuration

`ootrstats` requires a configuration file, which should be a [JSON](https://json.org/) object located at `$XDG_CONFIG_DIRS/ootrstats.json` on Unix, or `%APPDATA%\Fenhl\ootrstats\config\config.json` on Windows. It takes the following required entries:

* `workers`: An array of [worker configurations](#workers). You should specify at least one worker so seeds can be rolled.

## Workers

Each worker configuration is a JSON object with the following required entries:

* `name`: A string which will be displayed on the progress display on the command line, as well as in error messages.
* `kind`: One of the section headers listed below.

Depending on the `kind`, there are additional entries:

### `local`

A worker that runs on the same computer as the supervisor program. It takes the following additional required entry:

* `baseRomPath`: An absolute path to the vanilla OoT rom. See [the randomizer's documentation](https://github.com/OoTRandomizer/OoT-Randomizer#installation) for details.

And the following optional entries:

* `wslBaseRomPath`: An absolute path to the vanilla OoT rom that will be used if the randomizer is run inside [WSL](https://learn.microsoft.com/windows/wsl/about), i.e. for the `bench` subcommand if this worker is running on Windows. Defaults to `baseRomPath` if not specified.
* `cores`: The maximum number of instances of the randomizer to run in parallel. If this number is 0 or negative, it will be added to the number of available CPU cores, e.g. if 6 cores are detected and a number of `-2` is given, 4 cores will be used. Defaults to `-1`.

# Usage

Run the `ootrstats-supervisor` command, followed by any options you would like to change from their defaults, followed by a subcommand.

The supervisor can be interrupted cleanly using <kbd>Ctrl</kbd><kbd>D</kbd>. If this is used, the supervisor will wait for seeds currently being rolled to finish before exiting, but will no longer start rolling any new seeds.

## Options

* `--num-seeds`: Specifies the sample size, i.e. how many seeds to roll. Any existing seeds will be reused. Defaults to 16384.
* `--preset`: The name of the settings preset to use. Defaults to the Default/Beginner preset.

## Subcommands

### `bench`

Benchmarks the randomizer by measuring the average number of CPU instructions required to successfully generate a seed, taking into account the failure rate of the randomizer.

This subcommand requires workers to have access to [`perf`](https://perf.wiki.kernel.org/), which is only available for Linux. Workers running on Windows will attempt to use [WSL](https://learn.microsoft.com/windows/wsl/about). To install `perf` on an Ubuntu or Debian distro running inside WSL, run `apt-get install linux-tools-generic` and copy/symlink `/usr/lib/linux-tools/*-generic/perf` into your `PATH`.

Results will be displayed on stdout.
