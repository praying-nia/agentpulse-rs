# Relay production deployment

The production server owns its TLS certificate, private key, and
`/etc/agentpulse-relay/relay.json`. GitHub receives only a dedicated restricted
SSH key and deploys the already-tested release binary. Nginx is not involved;
Relay binds TCP 19191 directly.

## One-time server bootstrap

1. Generate a dedicated Ed25519 key with no passphrase for GitHub Actions. Do
   not reuse a personal or Host identity key.
2. Copy only its public key and this `deploy` directory to the server, then run:

   ```sh
   sudo ./bootstrap-server.sh ./agentpulse-github-deploy.pub
   ```

3. Install the certificate chain as `/etc/agentpulse-relay/fullchain.pem` and
   private key as `/etc/agentpulse-relay/privkey.pem`, owned by
   `root:agentpulse-relay`, with modes 0640 and 0640.
4. Install one release binary under `/opt/agentpulse-relay/releases/<sha>/`, set
   `/opt/agentpulse-relay/current` to that directory, then initialize once:

   ```sh
   sudo -u agentpulse-relay /opt/agentpulse-relay/current/agentpulse-relay init \
     --config /etc/agentpulse-relay/relay.json \
     --bind 0.0.0.0:19191 \
     --public-endpoint ap.nonamenona.top:19191 \
     --certificate-chain /etc/agentpulse-relay/fullchain.pem \
     --private-key /etc/agentpulse-relay/privkey.pem \
     --host-id <HOST_UUIDV7>
   ```

   Capture the Host enrollment Token exactly once and pipe it locally to
   `agentpulse relay configure --endpoint ap.nonamenona.top:19191 --token-stdin`.
   Then start the service with `sudo systemctl start agentpulse-relay`.

## GitHub production environment

Create an environment named `production`, optionally require an approver, and
add:

- Secret `RELAY_DEPLOY_SSH_KEY`: the dedicated private key, including its header
  and footer.
- Secret `RELAY_DEPLOY_KNOWN_HOSTS`: a pre-verified `known_hosts` line for the
  production SSH server. Do not generate this inside the deployment job.
- Variable `RELAY_DEPLOY_HOST`: SSH DNS name or IP.
- Variable `RELAY_DEPLOY_PORT`: SSH port, normally `22`.
- Variable `RELAY_ENDPOINT`: public Relay authority, currently
  `ap.nonamenona.top:19191`.

Every `master` push deploys only after the complete Rust CI job succeeds. The
server validates the staged binary and TLS configuration, atomically switches
the release symlink, restarts and probes the service, and restores the previous
symlink on failure. The Actions runner then performs a public TLS/Relay-v1 probe.

Certificate renewal is intentionally manual. Replace the two PEM files
atomically, run `agentpulse-relay check-config`, restart the service, and run a
public `probe`. `check-config` warns below 14 remaining days.
