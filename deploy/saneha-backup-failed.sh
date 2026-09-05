#!/bin/bash
# What a failed nightly backup does about itself.
#
# Started by `OnFailure=saneha-backup-failed.service` on saneha-backup.service,
# so it runs exactly when a backup run has failed and never otherwise. It is a
# host script and not a container: it has to be able to speak while the thing it
# reports on is broken, so it depends on nothing but curl, jq and logger, all of
# which quadhost already has.
#
# Install at: /usr/local/bin/saneha-backup-failed
# Unit:       /etc/systemd/system/saneha-backup-failed.service
#
# See docs/deploy.md, "When a backup fails".

set -uo pipefail

# The public name rather than the loopback: this is the same server every
# laptop talks to, and pointing it at 127.0.0.1 would make the notifier work
# only while the container on this host is the one serving.
SERVER="${SANEHA_URL:-https://saneha.clusterfault.com}"
CHANNEL="ops"
NAME="backup"
# `quadhost` on purpose, even though this box answers `hostname` with
# `hass.clusterfault.com` and `saneha` run here would derive `@hass`. The
# identity names the role and the machine as people talk about it, and the
# server only asks that a host be a slug. The two names for one box coexist:
# nothing but this unit ever claims `backup@quadhost`.
HOST="quadhost"
IDENTITY="${NAME}@${HOST}"
WATCHED="saneha-backup.service"
HEADLINE="@all saneha backup failed on quadhost: see journalctl -u saneha-backup"

# The journal first, and unconditionally. Everything below this line can fail —
# the server can be down, the route can be missing, DNS can be wrong — and the
# failure still lands somewhere a person can find it, on the host itself. This
# is the half that does not depend on saneha being able to run.
logger -p user.err -t saneha-backup-failed -- "$HEADLINE"

# The last lines of the run that failed, so the message says what happened and
# not only that something did. Capped well under MAX_BODY (64 KiB): the point is
# the first error, not the whole run.
#
# The tail goes inside a code fence, and this is not cosmetic. The server reads
# every `@word` in a body as a recipient (src/mention.rs) and refuses the whole
# message when one of them names nobody, so a single ` @foo` in a podman or
# mount error would throw away the only notification of a failed backup. Fenced
# blocks are skipped by that scanner. The fence is five backticks because a
# fence is closed only by a line of nothing but as many backticks again, and
# journal output does not produce one. The `@all` headline stays outside it,
# because that mention is the one that has to count.
FENCE='`````'
tail="$(journalctl -u "$WATCHED" -n 15 --no-pager -o short-iso 2>/dev/null | tail -c 4000)"
body="$HEADLINE"
if [ -n "$tail" ]; then
    body="${HEADLINE}"$'\n\n'"last lines of ${WATCHED}:"$'\n'"${FENCE}"$'\n'"${tail}"$'\n'"${FENCE}"
fi

# curl's exit code says whether the request happened at all; the status code
# says what the server thought of it. Both matter here, so the body and the
# status come back together and are split apart afterwards.
failed=0
post() {
    local path="$1" payload="$2" what="$3"
    shift 3
    local response rc status output accepted
    # This unit does not run again on its own (Restart=no, and the only thing
    # that starts it is a backup that already failed), so a blip on the way to
    # the server would be the end of the notification. Two retries cost nothing
    # and cover a server that is restarting as this runs.
    response="$(curl --silent --show-error --max-time 20 \
        --retry 2 --retry-connrefused \
        --write-out $'\n%{http_code}' \
        --header 'Content-Type: application/json' \
        --data-binary "$payload" \
        "${SERVER}${path}" 2>&1)"
    rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "${what}: could not reach ${SERVER}${path} (curl ${rc}): ${response}" >&2
        failed=1
        return 1
    fi
    status="${response##*$'\n'}"
    output="${response%$'\n'*}"
    for accepted in "$@"; do
        if [ "$status" = "$accepted" ]; then
            echo "${what}: ${status}"
            return 0
        fi
    done
    echo "${what}: ${SERVER}${path} answered ${status}: ${output}" >&2
    failed=1
    return 1
}

# All three steps are attempted whatever the earlier ones said, because one run
# of this should report everything wrong with the path rather than only the
# first thing. The exit code at the end is what fails the unit.

# 1. The channel. 409 is the server saying it is already there, which is the
#    normal case from the second failure onwards.
post "/channels" \
    "$(jq -nc --arg name "$CHANNEL" \
        '{name: $name, purpose: "host and service failures that need a person"}')" \
    "create channel ${CHANNEL}" 201 409

# 2. The identity. A join is idempotent in the way that matters here: an
#    existing participant is resumed (200) rather than refused, so this runs
#    every time. `harness` is `unknown` because this is a systemd unit and not
#    an agent, and `cwd` is / for the same reason. `same_host_session_live` is
#    false because no harness session on this host holds this identity — nothing
#    but this unit ever claims it.
post "/channels/${CHANNEL}/participants" \
    "$(jq -nc --arg name "$NAME" --arg host "$HOST" \
        '{name: $name, host: $host, harness: "unknown", cwd: "/",
          same_host_session_live: false}')" \
    "join ${CHANNEL} as ${IDENTITY}" 200 201

# 3. The message. `to: ["all"]` is what `saneha send --to all` sends, and the
#    body carries the @all mention as well, so the transcript reads the way it
#    would if a person had written it.
post "/channels/${CHANNEL}/messages" \
    "$(jq -nc --arg from "$IDENTITY" --arg body "$body" \
        '{from: $from, body: $body, to: ["all"]}')" \
    "post to ${CHANNEL}" 201

if [ "$failed" -ne 0 ]; then
    echo "the backup failure is in the journal but did not reach ${SERVER}" >&2
    exit 1
fi
echo "reported the backup failure to ${SERVER} as ${IDENTITY} in #${CHANNEL}"
