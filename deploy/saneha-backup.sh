#!/bin/sh
# Nightly backup of the saneha database to satyanas.
#
# Runs inside the one-shot saneha-backup container, which mounts:
#
#   /data     the live systemd-saneha-data volume, read-only
#   /backups  satyanas:/mnt/pool/container-volumes/saneha/backups, read-write
#
# Every failure exits non-zero, which fails saneha-backup.service — a backup
# that quietly wrote nothing is worse than no backup, because you stop looking.

set -eu
set -o pipefail

RETENTION_DAYS=14

# A copy is never overwritten. Two runs in one day are normal — a schema-changing
# deploy takes one before the restart and one after — and both would land on the
# same dated name, so the second run used to destroy the first. That is exactly
# what nearly happened on the migration-4 deploy, where the only pre-migration
# copy survived because an operator renamed it by hand.
#
# So: the dated name if it is free, and saneha-<date>-<HHMMSS>.db if it is not.
# One reading of the clock rather than two, because a second `date` call could
# land on the next day if the run crossed midnight between them.
NOW="$(date -u +%F-%H%M%S)"
DEST="/backups/saneha-${NOW%-*}.db"
if [ -e "$DEST" ]; then
    DEST="/backups/saneha-${NOW}.db"
fi
if [ -e "$DEST" ]; then
    echo "${DEST} already exists — refusing to overwrite a copy" >&2
    exit 1
fi

# If the volume were ever empty, or mounted somewhere other than where the
# server writes, sqlite3 would create a database there and take a flawless
# backup of nothing — the one failure that would pass every check below.
if [ ! -s /data/saneha.db ]; then
    echo "no database at /data/saneha.db — refusing to back up an empty one" >&2
    exit 1
fi

# .backup is SQLite's online backup API: a consistent copy of a database that is
# being written to while it is copied, which a cp of a WAL database is not.
#
# /data is mounted read-only, and which of the two ways in works depends on what
# the server has left on the volume:
#
#   -wal and -shm both present — the server is running, or was killed. SQLite
#     builds its wal-index in heap memory from the -shm it can read but not
#     write, and reads straight through the WAL. This is the normal nightly
#     case. If only one of the two is there it cannot, and fails.
#
#   neither present — the database was closed cleanly, which is what
#     `systemctl stop saneha.service` leaves. The header still says WAL, so an
#     ordinary open tries to create the -wal and fails on a read-only mount.
#     immutable=1 promises SQLite the file is not changing so it skips that.
#
# immutable=1 is a promise, and a wrong one is silent: pointed at a database
# with a WAL, it reads the file and ignores everything in the WAL, so the copy
# comes back quietly stale. So it is used only when there is no -wal, and the
# check afterwards throws the copy away if one appeared while we worked.
if [ -e /data/saneha.db-wal ]; then
    source_db="/data/saneha.db"
else
    source_db="file:/data/saneha.db?immutable=1"
fi

# No rm of $DEST first: it does not exist, and the checks above are what keep it
# that way. sqlite3 .backup creates it.
sqlite3 "$source_db" ".backup '${DEST}'"

if [ "$source_db" != "/data/saneha.db" ] && [ -e /data/saneha.db-wal ]; then
    echo "the server opened the database while it was being copied as immutable;" \
         "${DEST} may be missing writes, so it is not being kept" >&2
    rm -f "$DEST"
    exit 1
fi

# The copy inherits journal_mode=wal from the database it came from, which would
# make it a three-file thing that cannot even be read from a read-only mount.
# Turning it back to rollback mode makes each copy one self-contained file, and
# keeps the integrity_check below from leaving -wal and -shm on the NFS share.
# The server sets WAL again on open (src/store.rs), so a restored copy converts
# itself back.
sqlite3 "$DEST" 'PRAGMA journal_mode=DELETE;' > /dev/null

# A copy that cannot be read back is not a backup.
check="$(sqlite3 "$DEST" 'PRAGMA integrity_check;')"
if [ "$check" != "ok" ]; then
    echo "integrity_check on ${DEST} said: ${check}" >&2
    exit 1
fi
echo "wrote ${DEST}, $(wc -c < "$DEST") bytes, integrity_check ok"

# Attachments are opaque blobs the database points at, so they travel with it.
# The copy is additive, and exempt from the retention below: a database copy
# from ten days ago still references attachments deleted since, so mirroring
# deletions — or ageing the blobs out — would restore it into dangling
# references.
if [ -d /data/attachments ]; then
    mkdir -p /backups/attachments
    cp -a /data/attachments/. /backups/attachments/
    echo "copied /data/attachments -> /backups/attachments"
fi

# Dated copies, RETENTION_DAYS of them, in both the shapes this script writes:
# one run a day leaves saneha-YYYY-MM-DD.db, and every run after the first that
# day leaves saneha-YYYY-MM-DD-HHMMSS.db. Both age out together.
#
# The globs are the date shape rather than saneha-*.db so that the copies kept
# deliberately are never aged out by a later nightly run: saneha-pre-restore-*
# from a restore, saneha-pre-<what>-<date>.db from a schema-changing deploy.
# Both begin `saneha-pre`, and `p` is not [0-9], so neither pattern can reach
# them. Those are deleted by hand.
NIGHTLY='saneha-[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9].db'
NIGHTLY_TIMED='saneha-[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]-[0-9][0-9][0-9][0-9][0-9][0-9].db'

echo "pruning copies older than ${RETENTION_DAYS} days:"
find /backups -maxdepth 1 -type f \
    \( -name "$NIGHTLY" -o -name "$NIGHTLY_TIMED" \) \
    -mtime "+${RETENTION_DAYS}" -print -delete

echo "copies on satyanas now:"
find /backups -maxdepth 1 -type f \
    \( -name "$NIGHTLY" -o -name "$NIGHTLY_TIMED" \) | sort
