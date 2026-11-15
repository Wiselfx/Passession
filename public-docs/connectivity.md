---
title: Connectivity
description: Connect a Paseo client to your daemon through SSH, the relay, or Tailscale.
nav: Connectivity
order: 4
category: Getting started
---

# Connectivity

The native client connects to the Rust daemon on your desktop computer or server. Linux, macOS, and Windows clients can use a local daemon or remote hosts; Android and iOS are remote-only clients. Desktop and the Rust CLI can tunnel through SSH; mobile can use the encrypted relay or a direct connection over Tailscale. There is no daemon-served web product client.

On the daemon host, use `paseo onboard` for explicit setup, then `paseo daemon start` and `paseo daemon status`. Bare `paseo` opens chat. Normal operation uses port `6767`; repository development defaults to `.dev/paseo-home` and `6768`. This native migration is unpublished; see [Updates](/docs/updates) for local build availability.

This is client-to-daemon transport. If you are looking for the service that starts agents from GitHub, Slack, and Discord events, that is [Hub](/docs/hub).

- [SSH](#ssh)
- [Paseo relay](#paseo-relay)
- [Tailscale](#tailscale)

## SSH

SSH transport connects to an existing daemon through your local OpenSSH client. It does not install, start, or configure Paseo on the remote host.

Before connecting:

1. Start the Paseo daemon on the remote host.
2. Confirm `ssh user@host` works with a key or SSH agent. Paseo uses non-interactive SSH and follows your OpenSSH config.

The CLI accepts an SSH URI as its host:

```bash
paseo --host ssh://user@host ls -a
```

The daemon is expected at `127.0.0.1:6767` on the remote host. The port in the SSH URL is the SSH server port:

```bash
paseo --host ssh://user@host:2222 ls -a
```

Set a different remote daemon port with `daemonPort`:

```bash
paseo --host 'ssh://user@host?daemonPort=7777' ls -a
```

Put `--host` before the command. `paseo daemon status` observes the default local home; use `paseo --host ssh://user@host daemon status` to query a remote daemon. `paseo --host ssh://user@host run --cwd /path/on/remote ...` requires a working directory that exists on the remote host.

In Paseo Desktop, open **Settings → Add host → Remote SSH** and enter the same `ssh://` destination, including `:port` or `?daemonPort=` when those differ from the defaults.

If the remote daemon has a password, enter it in **Daemon password**; it is stored with the host and sent on every connection, the same as a direct connection's password. SSH login itself stays key-based — Paseo never prompts for an SSH password.

## Paseo relay

The relay works without Tailscale, port forwarding, or network configuration. Traffic is end-to-end encrypted.

Relay is disabled until you enable it.

### Enable relay from Paseo Desktop

1. Open **Settings → your host → Pair a device**.
2. Select **Enable relay**.
3. Scan the QR code with Paseo on your phone, or copy the pairing link and paste it into the phone app.

### Enable relay from the CLI

Run:

```bash
paseo daemon pair
```

Confirm when prompted. Paseo prints a QR code and pairing link. Scan the QR code with Paseo on your phone, or choose **Paste pairing link** in the phone app.

## Tailscale

The native app can join your tailnet itself using embedded libtailscale. This does **not** enable a phone-wide VPN or change other apps' network access. Normal Passession hosts and external internet connections still work while it is enabled.

Install [Tailscale](https://tailscale.com/download) on the **daemon machine** and sign it into your tailnet. The app's embedded node does not automatically expose a daemon bound only to `127.0.0.1`.

### 1. Find the daemon machine's Tailscale IP

Run this on the daemon machine:

```bash
tailscale ip -4
```

Copy the address it prints. The example below uses `100.101.102.103`.

### 2. Configure the daemon

Open `~/.paseo/config.json` and set `daemon.listen` to the Tailscale IP:

```json
{
  "$schema": "https://paseo.sh/schemas/paseo.config.v1.json",
  "version": 1,
  "daemon": {
    "listen": "100.101.102.103:6767"
  }
}
```

Keep the other settings already in the file. If it has a `daemon` object, add `listen` inside that object.

To restrict access with a password, see [Password authentication](/docs/configuration#password-authentication).

Restart the daemon:

```bash
paseo daemon restart
```

If Paseo Desktop manages the daemon, use **Settings → your host → Overview → Restart daemon**.

### 3. Connect the phone app

1. In Passession, open **Settings → Tailscale** and enable **Enable embedded Tailscale**.
2. Select **Log in with browser**, approve the app's node in your Tailscale account, then return and refresh status. Complete device approval in the tailnet admin console if required.
3. Open **Settings → Add host → Direct connection** and select **Embedded Tailscale** under **Connection network**.
4. Enter the daemon's Tailscale IP or MagicDNS name and port `6767`. Use TLS only if that daemon endpoint is configured for it.
5. Connect. Tailnet ACLs and the daemon machine's firewall must permit the app node to reach that port.

Only hosts selected as **Embedded Tailscale** use this app-owned node. Their failures do not silently switch to Normal or relay connections. Other hosts can remain **Normal** and use the ordinary network, including a separately installed system Tailscale app if desired. Embedded mode accepts direct `ws://` and `wss://` endpoints, not SSH, relay/pairing links or local IPC.

### Account and connection controls

**Settings → Tailscale** shows account, tailnet, device name, IPs and connection errors. **Disconnect** pauses the app's tailnet connections without stopping daemon sessions or forgetting login; **Reconnect** resumes them. **Log out** requests expiry of this app node's key and removes its local identity only after confirmation. Failed logout retains encrypted identity for retry. Removing the device's admin-console entry is a separate operation.

The device hostname is configurable. **Advanced control server** defaults to the official Tailscale service; a custom server requires HTTPS. Log out before changing servers. HTTP is accepted only for literal loopback development fixtures, not ordinary remote services.

Encrypted node state depends on OS secure storage: Linux Secret Service, Apple Keychain, Windows Credential Manager or Android Keystore. Unlock/restore the original store if it reports an error; copying only the encrypted state to another installation is not a portable login backup.

## Troubleshooting

- **SSH authentication failed:** Run `ssh user@host` in a terminal and fix the key, agent, host key, or `~/.ssh/config` entry there. Paseo does not prompt for SSH passwords.
- **SSH connects but Paseo is refused:** Run `paseo daemon status` on the remote host. SSH transport does not start the daemon.
- **SSH connects but Paseo reports "Password required":** The remote daemon is password-protected. Remove the SSH connection from the host and add it again, this time entering the daemon password in **Daemon password**.
- **Connection timed out:** Check the app's **Settings → Tailscale** status, the daemon machine's Tailscale connection, and tailnet ACL/firewall access. Confirm that this host is using the intended Normal or Embedded Tailscale mode.
- **Connection refused:** Run `paseo daemon status` and confirm the daemon is running on the configured IP and port.
- **Config change has no effect:** Run `paseo reload`. `daemon.listen` is a startup setting, so restart when the command reports it.
- **Embedded library missing:** Use a complete native app bundle built by `cargo xtask build-gui`; the standalone Rust GUI executable does not include its adjacent native library.
- **Secure storage unavailable:** Unlock the OS credential store. Linux additionally needs a running Secret Service and session bus. Passession does not fall back to plaintext keys.
- **Unknown embedded host:** Use a peer's Tailscale IP or MagicDNS name. Unrelated public hosts should use Normal mode; enabling Tailscale does not make the entire internet a tailnet route.
