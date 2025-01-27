#!/usr/bin/env pwsh

cargo check --workspace
if (-not $?)
{
    throw 'Native Failure'
}

# copy the tree to the WSL file system to improve compile times
wsl rsync --delete -av /mnt/c/Users/fenhl/git/github.com/fenhl/ootrstats/stage/ /home/fenhl/wslgit/github.com/fenhl/ootrstats/ --exclude target
if (-not $?)
{
    throw 'Native Failure'
}

wsl env -C /home/fenhl/wslgit/github.com/fenhl/ootrstats cargo check --workspace --exclude=ootrstats-worker-windows-service
if (-not $?)
{
    throw 'Native Failure'
}
