The file `assets/net.fenhl.ootrstats.plist` can be used as a starting point for creating a launch daemon that's automatically run Make sure to change the username in the path in `ProgramArguments`, then copy it to `/Library/LaunchDaemons`. It can then be loaded using `sudo launchctl load /Library/LaunchDaemons/net.fenhl.ootrstats.plist`.

To restart the launch daemon, run `sudo launchctl stop net.fenhl.ootrstats`.
