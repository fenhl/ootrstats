#!/usr/bin/env pwsh

cargo check --workspace
if (-not $?)
{
    throw 'Native Failure'
}

# copy the tree to the WSL file system to improve compile times
wsl -d ubuntu-m2 rsync --mkpath --delete -av /mnt/c/Users/fenhl/git/github.com/fenhl/ootrstats/stage/ /home/fenhl/wslgit/github.com/fenhl/ootrstats/ --exclude .cargo/config.toml --exclude target
if (-not $?)
{
    throw 'Native Failure'
}

wsl -d ubuntu-m2 env -C /home/fenhl/wslgit/github.com/fenhl/ootrstats /home/fenhl/.cargo/bin/cargo check --workspace --exclude=ootrstats-worker-windows-service
if (-not $?)
{
    throw 'Native Failure'
}

wsl nix build --no-link
if (-not $?)
{
    throw 'Native Failure'
}
