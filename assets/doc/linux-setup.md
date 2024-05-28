To get `perf` working on Linux (not inside WSL):

1. Get your kernel version with `uname -r`.
2. Run `sudo apt-get install linux-tools-A.B.C.D`, where `A.B.C.D` is your kernel version with the `-` between `C` and `D` replaced with `.`.
3. Run `sudo sysctl -w kernel.perf_event_paranoid=0` to allow `perf` to count instructions.
