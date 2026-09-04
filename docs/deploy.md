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
| Attachments | `/data/attachments/<channel id>/<attachment id>`, on the same volume |
| Name | `saneha.clusterfault.com`, LAN `192.168.16.169`, tailnet `100.81.17.63` |

There is no authentication (ADR-0003), so the server is published on loopback
only and Caddy is the one thing that reaches it. The name must never resolve to
a publicly routable address.

What is running right now is whatever tag the installed unit names:

```sh
ssh core@192.168.16.169 'grep ^Image= /etc/containers/systemd/saneha/saneha.container'
```

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
curl -s -H "Authorization: Bearer $(curl -s \
  'https://ghcr.io/token?scope=repository:surdy/saneha:pull' | jq -r .token)" \
  https://ghcr.io/v2/surdy/saneha/tags/list
```

That asks the registry anonymously, which is also the proof that quadhost can
pull. The `gh api /users/surdy/packages/...` route needs the `read:packages`
scope and 403s under the ordinary `gh` login.

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

Edit `Image=` in `deploy/saneha.container` to the tag you are deploying, and
commit that bump — on a branch, then merged. The repo copy is the record of
what is deployed, and the drift check at the end of this step only means
something if it is.

The unit no longer names `StopSignal`, so it relies on the server handling
SIGTERM ([issue #15](https://github.com/surdy/saneha/issues/15)): install it
only together with an image built from that fix or later, or every stop stalls
for `TimeoutStopSec` and ends in SIGKILL.

```sh
scp deploy/saneha.container deploy/saneha-data.volume core@192.168.16.169:/tmp/
ssh core@192.168.16.169 '
  sudo mkdir -p /etc/containers/systemd/saneha &&
  sudo install -m 0644 -o root -g root \
    /tmp/saneha.container /tmp/saneha-data.volume /etc/containers/systemd/saneha/ &&
  sudo systemctl daemon-reload &&
  sudo systemctl restart saneha.service'
```

`install` rather than `mv`: it writes a new inode under `/etc`, which SELinux
labels `etc_t`. A file moved from `/tmp` keeps `user_tmp_t`, and quadhost is
Enforcing.

`daemon-reload` is what regenerates the service from the Quadlet; a plain
restart without it runs the old definition. The volume outlives the unit, so
restarting and re-tagging never touch the database.

```sh
ssh core@192.168.16.169 'systemctl status saneha.service --no-pager'
ssh core@192.168.16.169 'journalctl -u saneha.service -n 30 --no-pager'
```

A stop should be quiet. `resorting to SIGKILL` or `status=137` in that journal
means the container is not taking the stop signal the unit names.

Last, check that the host and the repo have not drifted apart:

```sh
diff <(ssh core@192.168.16.169 sudo cat /etc/containers/systemd/saneha/saneha.container) \
     deploy/saneha.container
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

## Durability

Read this before assuming a transcript is safe.

`/data/saneha.db` sits on the `systemd-saneha-data` local volume, which is on
`/dev/sda4` — quadhost's single root disk. It is deliberately not on NFS from
satyanas, the way every other stateful service on the host is, because SQLite's
locking is not safe over NFS. **Nothing compensates for that yet: there is no
backup.** v1 has no retention or expiry either, so this file is the only copy of
every transcript, and a disk failure or an FCOS rebuild loses all of it.

Attachments are on the same volume and in the same gap. Their bytes live in
`/data/attachments/<channel id>/<attachment id>`, beside the database rather
than inside it, so a copy of `saneha.db` alone is not a copy of the channel: a
backup has to take that directory too, and a restore has to put both back
together or the transcript will name files that are not there. Issue #14 should
snapshot the database **first** and copy `attachments/` after it: a file is
written and fsynced before its row exists and is never modified afterwards, so
a database from before a file is consistent with the files from after it, and
the other order is not.

An attachment's file is fsynced, its directory is not, and SQLite runs at
`synchronous = NORMAL`, so a power cut can lose the row while leaving the file,
or lose a file that has no row yet. Neither loses an attachment a message
already carries: a message is written after its files. What is left over is an
orphan file, which the hourly sweep removes once it is more than an hour old.

A nightly `sqlite3 .backup` to satyanas, and the restore step to go with it, is
[issue #14](https://github.com/surdy/saneha/issues/14).

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
