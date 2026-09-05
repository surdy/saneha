"use strict";

// The viewer's read rule, driven by Node over a DOM small enough to fit in
// this file. Run by `tests/viewer.rs`, with the page as its one argument.
//
// What it holds the page to is the rule the README states: this page advances
// the person's read cursor when the tab is visible, the transcript is scrolled
// to its end, and the newest message has been there for a couple of seconds —
// and never otherwise. That is a `read` the person performed (ADR-0004), so
// what is asserted is which requests the page makes: exactly one POST to the
// cursor route, carrying the newest id, in the case that qualifies, and none
// at all in the four that do not.
//
// The script is taken out of the page and run as it ships. Nothing here
// renders anything: an element is a bag of properties, and what is asserted of
// the drawing is that the person's own marker carries the id that was sent.

const assert = require("assert");
const fs = require("fs");
const vm = require("vm");

const CHANNEL = "brisk-otter";
const OTHER = "quiet-heron";
const ME = "surdy@web";
const SETTLE = 2000; // what the page waits before it counts a message as read

const page = fs.readFileSync(process.argv[2], "utf8");
const script = page.slice(page.indexOf("<script>") + "<script>".length, page.lastIndexOf("</script>"));

/// One element: everything the page sets on one, and nothing else. The
/// transcript's three scroll numbers are what "scrolled to the end" is decided
/// from, so they are the ones a case sets.
function element(id) {
  const node = {
    id,
    value: "",
    textContent: "",
    title: "",
    innerHTML: "",
    hidden: false,
    dataset: {},
    scrollTop: 0,
    scrollHeight: 1000,
    clientHeight: 500,
    classList: { toggle() {} },
    listeners: {},
    addEventListener(type, fn) {
      (node.listeners[type] = node.listeners[type] || []).push(fn);
    },
    querySelectorAll: () => [],
    showModal() {},
    close() {},
    select() {},
    focus() {}
  };
  return node;
}

function message(id, from, body) {
  return {
    channel: CHANNEL,
    id,
    kind: "message",
    from,
    recipients: [],
    body,
    created_at: "2026-09-04T09:0" + id + ":00Z",
    attachments: []
  };
}

function participant(identity, read_cursor) {
  return {
    identity,
    name: identity.split("@")[0],
    host: identity.split("@")[1],
    harness: "web",
    cwd: null,
    away: false,
    read_cursor,
    joined_at: "2026-09-04T09:00:00Z"
  };
}

/// A page in a browser that does only what a case asks of it.
///
/// `joined` says whether this person is a participant, `name` whether they
/// have set one at all, `scrollTop` where the transcript is, and `later` the
/// messages the held polls bring in one at a time, which is how a burst is
/// made. Nothing is answered on a timer: the fetches resolve at once and the
/// page's own timers are recorded rather than run, so a case fires the read
/// itself and the two seconds cost nothing.
function world(options) {
  const elements = new Map();
  const timers = new Map();
  const requests = [];
  const cursors = [];
  const later = (options.later || []).slice();
  // What each channel holds. The one the page opens is longer than the other,
  // so a read after a switch is to an id the page has already read past —
  // which it makes anyway, because the id it has read to went with the
  // channel it read it in.
  const transcripts = {
    [CHANNEL]: [1, 2, 3].map((id) => message(id, "alpha@macbookpro", "in " + CHANNEL)),
    [OTHER]: [1, 2].map((id) => message(id, "alpha@macbookpro", "in " + OTHER))
  };
  let failCursor = options.failCursor || 0;
  let nextTimer = 1;

  const byId = (id) => {
    if (!elements.has(id)) elements.set(id, element(id));
    return elements.get(id);
  };

  const document = {
    hidden: false,
    title: "",
    listeners: {},
    getElementById: byId,
    addEventListener(type, fn) {
      (document.listeners[type] = document.listeners[type] || []).push(fn);
    }
  };

  const people = () => ({
    participants: [participant("alpha@macbookpro", 0)].concat(
      options.joined === false ? [] : [participant(ME, options.cursor || 0)]
    )
  });

  const json = (body) => Promise.resolve({
    ok: true,
    status: 200,
    text: () => Promise.resolve(JSON.stringify(body))
  });

  /// The server refusing, in its own words, which is what `api` reads.
  const refusal = () => Promise.resolve({
    ok: false,
    status: 500,
    text: () => Promise.resolve(JSON.stringify({ error: "the cursor did not move" }))
  });

  /// Which channel a request is about, so a case can switch between two.
  const about = (url) => (url.match(/^\/channels\/([^/?]+)/) || [])[1];

  function fetch(url, init) {
    requests.push({ url, init });
    // The channel list, with or without the `as` the page adds when it knows
    // who is reading: a server that carries per-channel unread answers with
    // that person's read cursor, and this one answers the same either way.
    if (url.split("?")[0] === "/channels") {
      return json({
        channels: [
          { name: CHANNEL, state: "open", purpose: "" },
          { name: OTHER, state: "open", purpose: "" }
        ]
      });
    }
    if (/\/cursor$/.test(url)) {
      const sent = JSON.parse(init.body);
      cursors.push({ url, channel: about(url), read_cursor: sent.read_cursor });
      if (failCursor > 0) {
        failCursor -= 1;
        return refusal();
      }
      return json(participant(ME, sent.read_cursor));
    }
    if (/\/messages\?.*hold=/.test(url)) {
      // A poll the server holds. One message per answer until the case runs
      // out of them, and then a hold that never comes back, which is where
      // the follower parks for the rest of the case.
      if (!later.length) return new Promise(() => {});
      const arriving = later.shift();
      transcripts[about(url)] = transcripts[about(url)].concat([arriving]);
      return json({ messages: [arriving] });
    }
    if (/\/messages\?/.test(url)) return json({ messages: transcripts[about(url)] });
    if (/\/participants$/.test(url)) return json(people());
    if (/^\/channels\/[^/]+$/.test(url)) {
      return json({ channel: { name: about(url), state: "open", purpose: "", created_at: "2026-09-04T09:00:00Z" } });
    }
    throw new Error("the page asked for " + url + ", which this DOM knows nothing about");
  }

  const listeners = {};
  const sandbox = {
    document,
    window: {
      addEventListener(type, fn) {
        (listeners[type] = listeners[type] || []).push(fn);
      }
    },
    location: { pathname: "/c/" + CHANNEL, reload() {} },
    history: { pushState() {} },
    localStorage: {
      getItem: () => (options.name === null ? null : "surdy"),
      setItem() {}
    },
    fetch,
    AbortController,
    confirm: () => false,
    console,
    setTimeout(fn, ms) {
      const id = nextTimer++;
      timers.set(id, { fn, ms });
      return id;
    },
    clearTimeout(id) {
      timers.delete(id);
    },
    setInterval: () => 0,
    clearInterval() {}
  };
  sandbox.window.document = document;
  // Where the transcript is when the page opens: at the end, as a browser
  // leaves a box nobody has scrolled, unless the case says otherwise.
  byId("transcript").scrollTop = options.scrolledUp ? 0 : 1000;

  return {
    sandbox,
    document,
    requests,
    cursors,
    el: byId,
    /// The page taken to another channel, the way the back button takes it:
    /// the path changes and `popstate` arrives.
    switchTo(name) {
      sandbox.location.pathname = "/c/" + name;
      for (const fn of listeners.popstate || []) fn();
    },
    /// The transcript being scrolled, which is what re-arms a read for a
    /// person who had scrolled away, or whose last one failed.
    scroll() {
      for (const fn of byId("transcript").listeners.scroll || []) fn();
    },
    /// The read the page is waiting to make, if it is waiting to make one.
    /// There is at most one: a burst restarts the wait rather than adding to
    /// it, which is the debounce this asserts.
    pendingReads() {
      return [...timers.values()].filter((timer) => timer.ms === SETTLE);
    },
    /// The couple of seconds elapsing.
    fireRead() {
      const [id, timer] = [...timers.entries()].find(([, t]) => t.ms === SETTLE) || [];
      assert.ok(timer, "the page never scheduled a read");
      timers.delete(id);
      timer.fn();
    },
    /// Lets everything the page has in flight resolve. Every fetch here
    /// answers at once, so this is a handful of turns of the loop and not a
    /// wait on anything.
    async settle() {
      for (let turn = 0; turn < 20; turn += 1) {
        await new Promise((done) => setTimeout(done, 0));
      }
    }
  };
}

/// Opens the page in one, and lets it catch up.
async function open(options) {
  const it = world(options || {});
  vm.runInNewContext(script, it.sandbox);
  await it.settle();
  return it;
}

async function main() {
  // Following a channel moves nothing by itself: the page's poll carries no
  // identity, and until the newest message has settled there is no read.
  const visible = await open({});
  assert.deepStrictEqual(visible.cursors, [], "following a channel moved a cursor on its own");
  assert.strictEqual(visible.pendingReads().length, 1, "no read was waiting to be made");
  assert.strictEqual(visible.pendingReads()[0].ms, SETTLE, "the read did not wait");

  // Visible, scrolled to the end, joined: the read the person performed.
  visible.fireRead();
  await visible.settle();
  assert.strictEqual(visible.cursors.length, 1, "the read was not made");
  assert.strictEqual(
    visible.cursors[0].url,
    "/channels/" + CHANNEL + "/participants/" + encodeURIComponent(ME) + "/cursor",
    "the read went somewhere else"
  );
  assert.strictEqual(visible.cursors[0].read_cursor, 3, "the read was not to the newest message");
  // And the person's own marker moves, like anyone else's.
  assert.ok(
    visible.el("peopleList").innerHTML.includes("read #3"),
    "the panel did not show the person's own cursor"
  );
  assert.ok(
    visible.el("transcript").innerHTML.includes("surdy has read to here"),
    "the transcript did not draw the person's own marker"
  );

  // A tab in the background is not somebody reading.
  const hidden = await open({});
  hidden.document.hidden = true;
  hidden.fireRead();
  await hidden.settle();
  assert.deepStrictEqual(hidden.cursors, [], "a hidden tab moved a cursor");

  // Nor is a reader scrolled back into history: the newest message is not on
  // the screen they are looking at.
  const back = await open({ scrolledUp: true });
  back.fireRead();
  await back.settle();
  assert.deepStrictEqual(back.cursors, [], "a reader scrolled up moved a cursor");

  // Somebody who has not joined has no cursor to move, and reading a page
  // announces nobody.
  const stranger = await open({ joined: false });
  stranger.fireRead();
  await stranger.settle();
  assert.deepStrictEqual(stranger.cursors, [], "a page that had not joined moved a cursor");

  // Neither does a page with no name set: there is no identity to read for.
  const nameless = await open({ name: null });
  nameless.fireRead();
  await nameless.settle();
  assert.deepStrictEqual(nameless.cursors, [], "a page with no name moved a cursor");

  // A burst is one read, at the end of it: three messages land one after
  // another, each restarts the wait, and what is sent is the newest id once.
  const burst = await open({
    later: [
      message(4, "alpha@macbookpro", "four"),
      message(5, "alpha@macbookpro", "five"),
      message(6, "alpha@macbookpro", "six")
    ]
  });
  assert.strictEqual(burst.pendingReads().length, 1, "a burst left more than one read waiting");
  assert.deepStrictEqual(burst.cursors, [], "a burst read before it had settled");
  burst.fireRead();
  await burst.settle();
  assert.strictEqual(burst.cursors.length, 1, "a burst was more than one read");
  assert.strictEqual(burst.cursors[0].read_cursor, 6, "the read was not to the newest message");

  // And a cursor already there is not sent again: a page that settles with
  // nothing new to read says nothing.
  const caughtUp = await open({ cursor: 3 });
  caughtUp.fireRead();
  await caughtUp.settle();
  assert.deepStrictEqual(caughtUp.cursors, [], "a cursor already at the newest message was sent again");

  // A channel switch: a read is made in each, and what the page has read to
  // in one is nothing to the other — the second channel is shorter, and is
  // still read to its own end.
  const switched = await open({});
  switched.fireRead();
  await switched.settle();
  assert.strictEqual(switched.cursors.length, 1, "the read in the first channel was not made");
  assert.strictEqual(switched.cursors[0].channel, CHANNEL);
  switched.switchTo(OTHER);
  await switched.settle();
  assert.strictEqual(
    switched.pendingReads().length,
    1,
    "the read waiting on the channel that was left is still waiting"
  );
  switched.fireRead();
  await switched.settle();
  assert.strictEqual(switched.cursors.length, 2, "the read in the channel switched to was not made");
  assert.strictEqual(switched.cursors[1].channel, OTHER, "the read went to the channel that was left");
  assert.strictEqual(
    switched.cursors[1].read_cursor,
    2,
    "the id read in one channel was carried into another"
  );

  // A read the server refuses says nothing to the person and is not the last
  // word: the next time the transcript settles, it is made again.
  const refused = await open({ failCursor: 1 });
  refused.fireRead();
  await refused.settle();
  assert.strictEqual(refused.cursors.length, 1, "the refused read was not made");
  assert.ok(
    !refused.el("transcript").innerHTML.includes("read to here"),
    "a refused read drew a marker anyway"
  );
  refused.scroll();
  refused.fireRead();
  await refused.settle();
  assert.strictEqual(refused.cursors.length, 2, "a refused read was never tried again");
  assert.strictEqual(refused.cursors[1].read_cursor, 3, "the second try read something else");
  assert.ok(
    refused.el("transcript").innerHTML.includes("surdy has read to here"),
    "the second try did not move the marker"
  );

  console.log("the read rule holds: one read when it should, none when it should not");
}

main().catch((failure) => {
  console.error(failure && failure.message ? failure.message : failure);
  process.exit(1);
});
