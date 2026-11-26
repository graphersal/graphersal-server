# Running as a Service

`start`/`stop`/`status` are for a person at a terminal. A service manager runs `serve` in the
foreground instead: it keeps the process, its restarts and its log itself. `serve` stops
cleanly on SIGTERM (databases unloaded, `server.run` removed, the lock released) and writes the
log to stderr. Run it as the dedicated user that owns the directory (the directory's owner is
its operator: [Security Model](security.md#the-directory-owner-is-the-operator)).

## systemd

A system unit, `/etc/systemd/system/graphersal-server.service`:

```ini
[Unit]
Description=graphersal-server
After=network.target

[Service]
User=graphersal
Group=graphersal
ExecStart=/usr/local/bin/graphersal-server serve --dir /srv/graphersal --listen 127.0.0.1:8484
KillSignal=SIGTERM
TimeoutStopSec=120
Restart=on-failure
UMask=0077

[Install]
WantedBy=multi-user.target
```

Create the service user and the directory, then enable the unit:

```bash
sudo useradd --system --home-dir /srv/graphersal --shell /usr/sbin/nologin graphersal
sudo install -d -o graphersal -g graphersal -m 700 /srv/graphersal
sudo -u graphersal graphersal-server init /srv/graphersal
sudo systemctl daemon-reload
sudo systemctl enable --now graphersal-server
journalctl -u graphersal-server -f
```

`init` as the service user prints the first admin's token once on your terminal. The
first admin's token of a directory without `access.toml` lands in the journal: run
`graphersal-server init /srv/graphersal` as that user first (it prints the token once on the
terminal).

## launchd (macOS)

A plist, `~/Library/LaunchAgents/org.graphersal.server.plist` for a user agent, or
`/Library/LaunchDaemons/` with a `UserName` key for a daemon):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>org.graphersal.server</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/graphersal-server</string>
    <string>serve</string>
    <string>--dir</string><string>/Users/me/graphersal</string>
    <string>--listen</string><string>127.0.0.1:8484</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ExitTimeOut</key><integer>120</integer>
  <key>StandardErrorPath</key><string>/Users/me/graphersal/logs/server.log</string>
  <key>StandardOutPath</key><string>/Users/me/graphersal/logs/server.log</string>
</dict>
</plist>
```

`launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/org.graphersal.server.plist`; stop it
with `launchctl bootout gui/$(id -u)/org.graphersal.server` (SIGTERM). Create `logs/` first
(`mkdir -m 700 /Users/me/graphersal/logs`).

## status and stop under a service manager

`graphersal-server status --dir DIR` works on a server a service manager runs too; `stop` would
stop it as well, but the manager may restart it (systemd `Restart=on-failure` does not: the
stop is a clean exit).

## `start` over SSH and logging out

`graphersal-server start` runs the server in its own session (`setsid` on Unix), so closing the
terminal does not stop it. But on a Linux machine whose systemd-logind has
`KillUserProcesses=yes`, logging out of the SSH session kills every process of that login
session, the server included. The log then ends without a stop line, and the next start
reports an unclean shutdown.

Check the setting:

```bash
busctl get-property org.freedesktop.login1 /org/freedesktop/login1 org.freedesktop.login1.Manager KillUserProcesses
```

`b true` means processes started in an SSH session die at logout. Fixes, best first:

1. **A system unit** (above): the server is not part of any login session.
2. **A user unit with lingering**, without root for the unit itself. Lingering keeps your user's
   systemd instance running after logout (enabling it may need an administrator):

   ```bash
   loginctl enable-linger $USER
   systemd-run --user --unit graphersal-server graphersal-server serve --dir $HOME/graphersal
   systemctl --user status graphersal-server
   ```

   `systemd-run --user` starts the server under your user's systemd instance, outside the SSH
   session, so logind does not kill it at logout. Lingering alone is not enough for a server
   started by `graphersal-server start` inside the SSH session: that process still belongs to
   the session.

Without `KillUserProcesses=yes` (the default of most distributions), `start` over SSH survives
the logout as it is.
