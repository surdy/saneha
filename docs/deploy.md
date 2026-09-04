# Deploying saneha

saneha runs on quadhost as a Podman Quadlet, fronted by Caddy at
`saneha.clusterfault.com`. Everything the deploy needs lives in this repo: the
[Containerfile](../Containerfile), the workflow that publishes the image, and
the unit in [`deploy/`](../deploy).

| | |
|---|---|
| Image | `ghcr.io/surdy/saneha:sha-<short sha>` (public) |
| Host | quadhost, `ssh core@192.168.16.169` |
| Unit | `/etc/containers/systemd/saneha/saneha.container` |
| Volume unit | `/etc/containers/systemd/saneha/saneha-data.volume` |
| Port | container `7343`, published on `127.0.0.1:7343` |
| Database | `/data/saneha.db` on the `systemd-saneha-data` volume |
| Name | `saneha.clusterfault.com`, LAN `192.168.16.169`, tailnet `100.81.17.63` |

There is no authentication (ADR-0003), so the server is published on loopback
only and Caddy is the one thing that reaches it. The name must never resolve to
a publicly routable address.

## 1. Build and push the image

`.github/workflows/image.yml` builds `linux/amd64` and pushes to GHCR on every
push to `main`, and on manual dispatch. Nothing is built by hand — there is no
podman on the laptops.

```sh
gh workflow run image.yml --repo surdy/saneha        # or just push to main
gh run list --repo surdy/saneha -L 1
```

Each build publishes two tags: `sha-<short sha>`, which is immutable and is what
the unit pins to, and `main`, which moves. Read back the tag you are about to
deploy:

```sh
gh api /users/surdy/packages/container/saneha/versions \
  --jq '.[0].metadata.container.tags'
```

The GHCR package must be public, since quadhost pulls without credentials. A
package is private on first publish; make it public once, at
`https://github.com/users/surdy/packages/container/saneha/settings`.

## 2. Register the DNS name

Once only, and already done for `saneha`. DNS is GitOps through dnscontrol in
`clusterfault/quadhost`; the canonical runbook is
`docs/runbooks/register-subdomain.md` in that repo. In short: add `A("saneha",
QUADHOST_LAN)` to the UniFi block and `A("saneha", QUADHOST_TAILSCALE,
CF_PROXY_OFF)` to the Cloudflare block of `dnscontrol/dnsconfig.js`, both, then
push to `main` — pushing is what applies the records, via a self-hosted runner
on quadhost.

```sh
gh run list --repo clusterfault/quadhost -L 1                     # green?
ssh core@192.168.16.169 'getent hosts saneha.clusterfault.com'    # 192.168.16.169
```

## 3. Install or update the unit

Edit `Image=` in `deploy/saneha.container` to the tag you are deploying, then:

```sh
scp deploy/saneha.container deploy/saneha-data.volume core@192.168.16.169:/tmp/
ssh core@192.168.16.169 '
  sudo mkdir -p /etc/containers/systemd/saneha &&
  sudo mv /tmp/saneha.container /tmp/saneha-data.volume /etc/containers/systemd/saneha/ &&
  sudo systemctl daemon-reload &&
  sudo systemctl restart saneha.service'
```

`daemon-reload` is what regenerates the service from the Quadlet; a plain
restart without it runs the old definition. The volume outlives the unit, so
restarting and re-tagging never touch the database.

```sh
ssh core@192.168.16.169 'systemctl status saneha.service --no-pager'
ssh core@192.168.16.169 'journalctl -u saneha.service -n 30 --no-pager'
```

Caddy notices the container's labels by itself; it needs no restart, and its
configuration is never edited by hand. The first request for a new name waits
on the Let's Encrypt DNS-01 challenge, so give it a few seconds.

## 4. Verify

From either laptop, on the LAN or the tailnet:

```sh
curl https://saneha.clusterfault.com/health
# {"service":"saneha","status":"ok"}

export SANEHA_URL=https://saneha.clusterfault.com
saneha new --purpose "checking the deploy"
saneha list
```

## Installing the binary on a laptop

The laptops run the same binary as a client. There is no released artifact yet:

```sh
cargo install --path .          # into ~/.cargo/bin
export SANEHA_URL=https://saneha.clusterfault.com
```

Put the `SANEHA_URL` export in the shell profile so every agent on the machine
inherits it.

## When something is wrong

- **502 from Caddy** — the container is down or not listening. Check
  `systemctl status saneha.service`, then
  `ssh core@192.168.16.169 'curl -s localhost:7343/health'`.
- **Name does not resolve** — the dnscontrol run is red or was never pushed. See
  step 2; do not add records by hand in UniFi or Cloudflare.
- **`podman pull` denied on quadhost** — the GHCR package went private. See
  step 1.
- **Certificate errors** — Caddy needs `CLOUDFLARE_API_TOKEN`, which lives in
  its own environment file on quadhost. The label in the unit references it as
  `{$$CLOUDFLARE_API_TOKEN}`; the double `$` is intentional, because Podman
  eats a single one.
