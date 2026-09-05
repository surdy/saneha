# Deploying saneha

saneha runs on quadhost as a Podman Quadlet, fronted by Caddy at
`saneha.clusterfault.com`. Everything the deploy needs lives in this repo: the
[Containerfile](../Containerfile), the workflow that publishes the image, and
the units in [`deploy/`](../deploy).

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
| Backup | `saneha-backup.timer` nightly at 03:30 UTC, to satyanas |
| Copies | `satyanas:/mnt/pool/container-volumes/saneha/backups`, 14 days |

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

`/data/saneha.db` sits on the `systemd-saneha-data` local volume, which is on
`/dev/sda4` — quadhost's single root disk. It is deliberately not on NFS from
satyanas, the way every other stateful service on the host is, because SQLite's
locking is not safe over NFS. v1 has no retention or expiry either, so the live
file is the only *live* copy of every transcript.

What compensates is the nightly backup below: `saneha-backup.timer` fires at
03:30 UTC, takes a `sqlite3 .backup` of the running database, verifies it, and
leaves it on satyanas, where fourteen dated copies are kept. So a disk failure
or an FCOS rebuild costs at most a day. Two things it is not: it is not
continuous — anything written since the last run is gone — and both copies are
in the same house.

Attachments are on the same volume. Their bytes live in
`/data/attachments/<channel id>/<attachment id>`, beside the database rather
than inside it, so a copy of `saneha.db` alone is not a copy of the channel:
the backup below copies that directory too, and a restore that puts back only
the database leaves a transcript naming files that are not there.

The order matters and the backup has it right: the database is copied first and
`attachments/` after it. A file is written and fsynced before the row that
names it exists, and is never modified afterwards, so a database from before a
file is consistent with the files from after it. The other order is not.

An attachment's file is fsynced, its directory is not, and SQLite runs at
`synchronous = NORMAL`, so a power cut can lose a row while leaving its file,
or lose a file that has no row yet. Neither loses an attachment a message
already carries, because a message is written after its files. What is left
over is an orphan file, which the server's hourly sweep removes once it is more
than an hour old.

## Backup and restore

| | |
|---|---|
| Timer | `/etc/systemd/system/saneha-backup.timer`, `03:30` UTC + up to 10m |
| Unit | `/etc/containers/systemd/saneha/saneha-backup.container` |
| Script | `/etc/containers/systemd/saneha/saneha-backup.sh` |
| Volume unit | `/etc/containers/systemd/saneha/saneha-backups.volume` |
| Copies | `satyanas:/mnt/pool/container-volumes/saneha/backups/saneha-YYYY-MM-DD.db` |
| Retention | 14 days, pruned by the same run that writes |

quadhost has no `sqlite3`, and the copy must be taken with SQLite's online
backup API rather than `cp` — a `cp` of a database in WAL mode that is being
written to copies a torn file. So the run is a one-shot container built on a
digest-pinned Alpine image that does have `sqlite3`, with the live volume
mounted read-only at `/data` and the NFS volume at `/backups`. It backs up,
turns the copy back to rollback journalling so it is one self-contained file,
runs `PRAGMA integrity_check` on it, copies `/data/attachments` if that
directory exists, and prunes copies older than fourteen days. Anything that
goes wrong exits non-zero, and the unit fails.

Reading a WAL database from a read-only mount is the fiddly part, and it is
worth knowing which case you are in before believing a failure:

- **`-wal` and `-shm` both on the volume** — the server is running, or was
  killed. SQLite builds the wal-index in heap memory from the `-shm` and reads
  through the WAL. This is the normal nightly case, and it works.
- **Neither on the volume** — the database was closed cleanly, which is exactly
  what `systemctl stop saneha.service` leaves behind. The header still says WAL,
  so an ordinary open tries to create the `-wal` and fails with `unable to open
  database file`. The script opens `file:/data/saneha.db?immutable=1` in this
  case instead, and throws the copy away if a `-wal` appeared while it worked —
  `immutable=1` against a database that does have a WAL reads the file and
  ignores the WAL, so it would come back silently stale.
- **One of the two, not both** — nothing read-only can do anything with that.
  It fails, correctly. Starting `saneha.service` resolves the WAL and the next
  run succeeds.

The attachment copy is additive, and exempt from the fourteen-day retention:
nothing is deleted from `/backups/attachments` when it disappears from the live
tree, because a database copy from ten days ago still points at attachments
deleted since. The restore below does not put attachments back either — copy
them by hand if you need them.

On satyanas the copies live in their own dataset, `pool/container-volumes/saneha`,
exported over NFS to `192.168.16.169` only, `maproot=root` — the same shape as
every other container volume there.

### Install it

Once. The units are in [`deploy/`](../deploy) like the server's.

```sh
scp deploy/saneha-backup.container deploy/saneha-backups.volume \
    deploy/saneha-backup.sh deploy/saneha-backup.timer core@192.168.16.169:/tmp/
ssh core@192.168.16.169 '
  sudo install -m 0644 -o root -g root \
    /tmp/saneha-backup.container /tmp/saneha-backups.volume \
    /etc/containers/systemd/saneha/ &&
  sudo install -m 0755 -o root -g root /tmp/saneha-backup.sh \
    /etc/containers/systemd/saneha/ &&
  sudo install -m 0644 -o root -g root /tmp/saneha-backup.timer \
    /etc/systemd/system/ &&
  sudo systemctl daemon-reload &&
  sudo systemctl enable --now saneha-backup.timer'
```

The timer is a plain systemd unit and goes under `/etc/systemd/system`, not
under `/etc/containers/systemd`: Quadlet generates no timers. It fires
`saneha-backup.service`, which Quadlet does generate, from the `.container`
file.

### Check the last run

```sh
ssh core@192.168.16.169 'systemctl list-timers saneha-backup.timer --no-pager'
ssh core@192.168.16.169 'systemctl status saneha-backup.service --no-pager'
ssh core@192.168.16.169 'sudo journalctl -u saneha-backup.service -n 20 --no-pager'
```

Worth doing weekly: nothing on quadhost forwards a failure anywhere, so a unit
that has been failing since Tuesday looks exactly like one that has not run yet
until someone looks. Making the failure arrive on its own is
[issue #23](https://github.com/surdy/saneha/issues/23).

A good run says `wrote /backups/saneha-<date>.db, <n> bytes, integrity_check ok`
and then lists what is on satyanas. A bad one leaves the unit failed, so it also
shows up in `systemctl --failed` — which is the point of the checks in the
script. Or ask satyanas directly:

```sh
ssh admin@satyanas 'ls -la /mnt/pool/container-volumes/saneha/backups/'
```

To run one now, out of band: `sudo systemctl start saneha-backup.service`.

### Restore

This puts a copy back over the live database. It stops the server; run it
knowing that.

The copies are on an NFS volume that is only mounted while a container using it
runs, so there is no stable host path to `cp` from — the copy back happens
inside a throwaway container that mounts both volumes.

```sh
ssh core@192.168.16.169
IMG=$(grep ^Image= /etc/containers/systemd/saneha/saneha-backup.container | cut -d= -f2-)

# 1. What is there.
sudo podman run --rm --network none --user 0:0 --entrypoint /bin/sh \
  -v systemd-saneha-backups:/backups "$IMG" -c 'ls -la /backups'

# 2. Stop the server. It must not be holding the file you are replacing.
sudo systemctl stop saneha.service

# 3. Keep what is there now. Restoring the wrong date is a normal mistake, and
#    without this the live database is gone before you notice you made it.
sudo podman run --rm --network none --user 0:0 --entrypoint /bin/sh \
  -v systemd-saneha-backups:/backups \
  -v systemd-saneha-data:/data \
  "$IMG" -c '
    set -eux
    keep=/backups/saneha-pre-restore-$(date -u +%FT%H%M)
    cp /data/saneha.db "$keep.db"
    [ -e /data/saneha.db-wal ] && cp /data/saneha.db-wal "$keep.db-wal" || true
    ls -la /backups'

# 4. Put the chosen copy back. The -wal and -shm belong to the database being
#    replaced; left behind, SQLite would replay them over the copy.
sudo podman run --rm --network none --user 0:0 --entrypoint /bin/sh \
  -v systemd-saneha-backups:/backups \
  -v systemd-saneha-data:/data \
  "$IMG" -c '
    set -eux
    cp /backups/saneha-YYYY-MM-DD.db /data/saneha.db
    rm -f /data/saneha.db-wal /data/saneha.db-shm
    chown 10001:10001 /data/saneha.db'

# 5. Start it and look.
sudo systemctl start saneha.service
curl https://saneha.clusterfault.com/channels
```

The `saneha-pre-restore-*` copies are named differently from the nightly ones on
purpose: the prune only matches `saneha-YYYY-MM-DD.db`, so nothing ages them out
from under you. Delete them by hand once the restore has proved itself.

Files written on the volume from inside a container come out labelled
`container_file_t`, so nothing needs `restorecon` afterwards. The copy is in
rollback mode, so it is one file and reads fine anywhere, including from a `:ro`
mount; the server puts it back into WAL when it opens it.

### The drill

Run this instead when you want to know the backups are restorable without
touching the live one. It is the same procedure against a scratch volume, and
it ends by letting the real server image open what it restored.

```sh
IMG=docker.io/keinos/sqlite3@sha256:a5610a155a8c9007f2050120406a0abcffab246570d6ac1ffe370f5f23e14dc1
sudo podman volume create saneha-restore-drill

# The live volume's directory is already owned by 10001; a fresh one is not.
sudo podman run --rm --network none --user 0:0 --entrypoint /bin/sh \
  -v systemd-saneha-backups:/backups -v saneha-restore-drill:/data "$IMG" -c '
    set -eux
    chown 10001:10001 /data
    cp /backups/saneha-YYYY-MM-DD.db /data/saneha.db
    rm -f /data/saneha.db-wal /data/saneha.db-shm
    chown 10001:10001 /data/saneha.db'

sudo podman run --rm --network none --user 10001:10001 --entrypoint /bin/sh \
  -v saneha-restore-drill:/data "$IMG" -c '
    sqlite3 /data/saneha.db "PRAGMA integrity_check;"
    sqlite3 -header -column /data/saneha.db "select id, name, purpose, state from channels;"'

# The real server, on the restored copy, with no network and no published port.
sudo podman run -d --rm --name saneha-restore-drill --network none \
  --user 10001:10001 -v saneha-restore-drill:/data \
  ghcr.io/surdy/saneha:sha-9ede845 serve
sudo podman exec saneha-restore-drill curl -s http://127.0.0.1:7343/channels

sudo podman stop -t 5 saneha-restore-drill
sudo podman volume rm saneha-restore-drill
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
- **`saneha-backup.service` failed** — read
  `journalctl -u saneha-backup.service`. `unable to open database file` means
  the volume has one of `-wal`/`-shm` but not both, which nothing read-only can
  read through; starting `saneha.service` resolves the WAL and the next run
  succeeds. (A cleanly stopped server, which leaves neither file, is handled —
  see the WAL cases above.) A mount error instead means satyanas or the export is
  unreachable — check the share is still there with
  `ssh admin@satyanas 'midclt call sharing.nfs.query'`, and that it still lists
  `192.168.16.169` as a permitted host.
- **Certificate errors** — Caddy needs `CLOUDFLARE_API_TOKEN`, which lives in
  its own environment file on quadhost. The label in the unit references it as
  `{$$CLOUDFLARE_API_TOKEN}`; the double `$` is intentional, because Podman
  eats a single one.
