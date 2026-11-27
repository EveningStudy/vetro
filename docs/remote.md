# Remote access

The computer running Verre hosts the agent sessions. Open **Settings → Remote access** there, then choose a connection method. A paired browser can send messages, steer a running agent when supported, and answer its approval requests. Give each device its own pairing and revoke it from the host when needed.

If an agent needs sign-in, complete its sign-in on the host. Select **Check now** in the paired browser to refresh the agent's status.

Leave **Remember this browser** selected to reconnect from a new tab or after reopening the browser. On a shared browser, clear it to keep the session in the pairing tab. Existing tabs paired with an earlier version can keep using their current session. Pair again with the setting selected to remember that browser.

The pairing tab keeps draft text, file mentions, and pending send receipts when the browser rebuilds the page. Image attachments are kept in the current page. If a delivery receipt cannot be confirmed after a disconnect or host restart, Verre blocks automatic duplicates and offers **Send anyway** so you can explicitly resend verbatim after checking the thread history. Revoking a pairing clears its saved drafts when the browser detects the revocation.

Paired devices can only browse and preview files inside known project folders; host-only actions (adding projects, changing settings, running agent sign-in, opening external apps, or managing remote pairing) remain restricted to the computer running Verre. See [SECURITY.md](../SECURITY.md) for details.

In the desktop app, enable **Keep running when the window closes** to leave the host available after closing its window. Choose **Quit** from the tray or menu bar to stop the app.

## Tailscale Serve

Install Tailscale on the host and client and connect both to a network that lets them reach each other. Enable MagicDNS and HTTPS in the tailnet. In Verre, select **Start Tailscale Serve**, then **Pair a device**. Scan the QR code or open the copied link on the client. You can also open the displayed address and type the code. The code works once and expires after ten minutes.

Verre uses HTTPS port 8443 and leaves an existing service on that port alone. Tailscale's network access rules still apply. The forwarding process stops when remote access is disabled or Verre exits.

If the Serve process exits unexpectedly, Verre closes its remote connections and clears the pending pairing code. Start Tailscale Serve again from Remote access. Devices that were already paired can reconnect with their existing sessions.

If pairing times out, choose **Connect** again on the same page with the same code and device name. If the host already enrolled the device, the browser recovers that pairing.

## Your own HTTPS reverse proxy

Choose a hostname that resolves to your proxy from the devices on your private network, and configure a certificate those devices trust. Forward the whole origin, including WebSocket upgrades on `/api/ws`, to the loopback address displayed in Settings. Preserve the browser's `Host` header and let the proxy set `X-Forwarded-For` from the actual client address.

A minimal Caddy configuration, assuming a certificate is already available for your private DNS name:

```caddyfile
verre.example.net {
    tls /path/to/fullchain.pem /path/to/key.pem
    reverse_proxy 127.0.0.1:47600
}
```

In Verre, enter `https://verre.example.net` under **HTTPS reverse proxy**, select **Start**, and pair the client. The URL is an HTTPS origin with an optional port. Use the hostname and port the browser actually visits.

For unattended startup, run:

```sh
verre serve --port 47600 --remote-url https://verre.example.net
```

Open the local URL printed by that command on the host to manage pairing. For a proxy that targets the desktop app across restarts, set `"port": 47600` (or another free loopback port) in the Verre data directory's `config.json` before launching the app. A newly launched desktop instance starts with remote access off. Paired devices can reconnect after you reenable the same address.

## Disconnecting devices

**Stop remote access** closes remote connections and clears the active pairing code. Previously paired devices remain registered for the next start. **Revoke** deletes a device's session so it must pair again. A device can also revoke its own session. An independently managed proxy stays under its owner's control when Verre stops accepting remote traffic.

Verre binds to `127.0.0.1`. Use a private network or VPN to make the HTTPS endpoint reachable, and restrict which devices can reach it with your network's access rules.
