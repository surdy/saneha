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
STAMP="$(date -u +%F)"
DEST="/backups/saneha-${STAMP}.db"

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
# /data is mounted read-only and this still works on a live WAL database: SQLite
# falls back to read-only shared-memory mode, using the -shm file the running
# server holds open. The one case it does not cover is a -wal left behind by an
# unclean stop with the server down — then this fails loudly, and starting
# saneha.service recovers the WAL so the next run succeeds.
rm -f "$DEST"
sqlite3 /data/saneha.db ".backup '${DEST}'"

# A copy that cannot be read back is not a backup.
check="$(sqlite3 "$DEST" 'PRAGMA integrity_check;')"
if [ "$check" != "ok" ]; then
    echo "integrity_check on ${DEST} said: ${check}" >&2
    exit 1
fi
echo "wrote ${DEST}, $(wc -c < "$DEST") bytes, integrity_check ok"

# Attachments are opaque blobs the database points at, so they travel with it.
# The copy is additive on purpose: a database copy from ten days ago still
# references attachments deleted since, and mirroring deletions would restore it
# into dangling references.
if [ -d /data/attachments ]; then
    mkdir -p /backups/attachments
    cp -a /data/attachments/. /backups/attachments/
    echo "copied /data/attachments -> /backups/attachments"
fi

# Dated copies, RETENTION_DAYS of them.
echo "pruning copies older than ${RETENTION_DAYS} days:"
find /backups -maxdepth 1 -type f -name 'saneha-*.db' -mtime "+${RETENTION_DAYS}" -print -delete

echo "copies on satyanas now:"
find /backups -maxdepth 1 -type f -name 'saneha-*.db' | sort
