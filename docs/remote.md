# Remote access

The computer running Verre hosts every agent session. Open **Settings → Remote access** on the host and pick a connection method. Paired browsers can browse threads, send or steer prompts, answer approval requests, and view read-only diffs and files inside known project folders.

Host-only actions (adding or removing projects, changing settings, running agent sign-in, opening external apps, voice model setup, and managing pairings) stay restricted to the computer running Verre. See [SECURITY.md](../SECURITY.md) for the full boundary list.

## Basics

- **Host sign-in:** if an agent asks for sign-in, complete it on the host and click **Check now** in the remote browser.
- **Persistent vs. one-tab pairing:** leave **Remember this browser** checked to reconnect from new tabs, or uncheck it on a shared device to keep the session in the current tab only. Give each device its own pairing so you can revoke one without affecting others.
- **Disconnect resilience:** drafts, file mentions, and pending send receipts stay intact across page reloads; images remain in the current page. If a send cannot be confirmed after a host restart or drop, Verre avoids duplicate sends and offers **Send anyway**.
- **Background host:** in the desktop app, enable **Keep running when the window closes** so closing the window keeps the core online for paired devices. Use **Quit** in the tray or menu bar to shut down.

## Tailscale Serve

1. Install Tailscale on both devices on the same tailnet, with MagicDNS and HTTPS enabled.
2. In Verre, click **Start Tailscale Serve** → **Pair a device**.
3. Scan the QR code, open the copied link, or visit the shown address and enter the one-time code (expires in 10 minutes).

Verre serves over HTTPS port `8443` and leaves any existing service on that port untouched. Stopping remote access or quitting Verre tears down its Serve entry. Previously paired devices reconnect automatically once Serve is started again.

If pairing times out while the host already enrolled the device, click **Connect** again with the same code and device name to recover the session.

## Your own HTTPS reverse proxy

Point a private DNS name with a trusted TLS certificate at your reverse proxy, and forward HTTP plus WebSocket upgrades (`/api/ws`) to the loopback address shown in Settings. Keep the browser's `Host` header intact and let the proxy set `X-Forwarded-For`.

Minimal Caddy config:

```caddyfile
verre.example.net {
    tls /path/to/fullchain.pem /path/to/key.pem
    reverse_proxy 127.0.0.1:47600
}
```

Enter `https://verre.example.net` under **HTTPS reverse proxy**, click **Start**, and pair your client.

For unattended or headless use:

```sh
verre serve --port 47600 --remote-url https://verre.example.net
```

To keep the desktop app on a fixed loopback port across restarts, set `"port": 47600` in `~/.verre/config.json`. Desktop launches start with remote access turned off; once you click **Start** again with the same URL, already paired devices reconnect.

## Disconnecting devices

- **Stop remote access** closes active remote sessions and invalidates the current pairing code, while keeping paired devices registered.
- **Revoke** deletes a device's session token so it must pair again (a remote browser can also revoke itself).
- Verre only ever binds `127.0.0.1`; network reachability stays bounded by your tailnet or private proxy ACLs.
