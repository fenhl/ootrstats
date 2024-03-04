#!/usr/bin/env pwsh

function ThrowOnNativeFailure {
    if (-not $?)
    {
        throw 'Native Failure'
    }
}

cargo check --workspace
ThrowOnNativeFailure

# copy the tree to the WSL file system to improve compile times
wsl rsync --delete -av /mnt/c/Users/fenhl/git/github.com/fenhl/ootrstats/stage/ /home/fenhl/wslgit/github.com/fenhl/ootrstats/ --exclude target
ThrowOnNativeFailure

wsl env -C /home/fenhl/wslgit/github.com/fenhl/ootrstats cargo check --workspace
ThrowOnNativeFailure
