"use strict";

// The rail, the palette and the purpose line, driven by Node over a DOM small
// enough to fit in this file. Run by `tests/viewer.rs`, with the page as its
// one argument.
//
// What it holds the page to is what the redesign decided and what a browser
// would otherwise be the only witness of: a hash in front of every channel
// name, pinned channels first in the order this browser keeps them, unread
// carried by weight as well as by a badge, a closed channel marked with the
// struck hash and never with a padlock, a palette that filters the list the
// page is already holding, and a purpose edited in place against PATCH.
//
// The script is taken out of the page and run as it ships. Nothing here
// renders anything: an element is a bag of properties, and what is asserted is
// the HTML the page puts into them and the requests it makes.

const assert = require("assert");
const fs = require("fs");
const vm = require("vm");

const ME = "surdy@web";

const page = fs.readFileSync(process.argv[2], "utf8");
const script = page.slice(page.indexOf("<script>") + "<script>".length, page.lastIndexOf("</script>"));

/// The channels this world serves, with the unread the page draws a badge
/// from: `newest_id` less `read_cursor`, as `GET /channels?as=` carries them.
const CHANNELS = [
  { name: "xlaptop-1", state: "open", purpose: "cross-laptop wake test", newest_id: 6, read_cursor: 6 },
  { name: "ops", state: "open", purpose: "backup failures land here", newest_id: 13, read_cursor: 1 },
  { name: "brisk-otter", state: "open", purpose: "the auth refactor", newest_id: 4, read_cursor: 1 },
  { name: "madari-relay", state: "open", purpose: null, newest_id: 0, read_cursor: 0 },
  { name: "notes-method", state: "closed", purpose: "how the vault gets written", newest_id: 1, read_cursor: 0 },
  { name: "deploy-quadhost", state: "open", purpose: "rolling it out", newest_id: 0, read_cursor: 0 }
];

function element(id) {
  const node = {
    id,
    value: "",
    textContent: "",
    title: "",
    innerHTML: "",
    hidden: false,
    disabled: false,
    dataset: {},
    style: {},
    scrollTop: 0,
    scrollHeight: 1000,
    clientHeight: 500,
    classes: new Set(),
    classList: {
      toggle: (name, on) => (on ? node.classes.add(name) : node.classes.delete(name)),
      add: (name) => node.classes.add(name),
      remove: (name) => node.classes.delete(name),
      contains: (name) => node.classes.has(name)
    },
    listeners: {},
    addEventListener(type, fn) {
      (node.listeners[type] = node.listeners[type] || []).push(fn);
    },
    fire(type, event) {
      for (const fn of node.listeners[type] || []) fn(event);
    },
    querySelectorAll: () => [],
    querySelector: () => null,
    getBoundingClientRect: () => ({ top: 0, bottom: 0, left: 0, right: 0, width: 0, height: 0 }),
    scrollIntoView() {},
    append() {},
    remove() {},
    showModal() {},
    close() {},
    select() {},
    focus() {}
  };
  return node;
}

function world(options) {
  options = options || {};
  const elements = new Map();
  const requests = [];
  const byId = (id) => {
    if (!elements.has(id)) elements.set(id, element(id));
    return elements.get(id);
  };

  const document = {
    hidden: false,
    title: "",
    listeners: {},
    activeElement: null,
    getElementById: byId,
    querySelector: () => null,
    createElement: () => element("made"),
    body: { append() {} },
    addEventListener(type, fn) {
      (document.listeners[type] = document.listeners[type] || []).push(fn);
    }
  };

  const json = (body) => Promise.resolve({
    ok: true,
    status: 200,
    text: () => Promise.resolve(JSON.stringify(body))
  });

  const about = (url) => (url.match(/^\/channels\/([^/?]+)/) || [])[1];
  const held = () => new Promise(() => {});

  function fetch(url, init) {
    requests.push({ url, init });
    if (url.split("?")[0] === "/channels") return json({ channels: CHANNELS });
    if (/\/messages\?.*hold=/.test(url)) return held();
    if (/\/messages\?/.test(url)) return json({ messages: [] });
    if (/\/participants$/.test(url)) {
      return json({
        participants: [{
          identity: ME, name: "surdy", host: "web", harness: "web",
          cwd: null, away: false, read_cursor: 0, joined_at: "2026-09-06T09:00:00Z"
        }]
      });
    }
    if (/^\/channels\/[^/]+$/.test(url) && (!init || (init.method || "GET") === "GET")) {
      const found = CHANNELS.find((channel) => channel.name === about(url));
      return json({ channel: found });
    }
    // The purpose being set: the one PATCH the page makes.
    if (/^\/channels\/[^/]+$/.test(url) && init.method === "PATCH") {
      const sent = JSON.parse(init.body);
      const found = CHANNELS.find((channel) => channel.name === about(url));
      return json(Object.assign({}, found, { purpose: sent.purpose }));
    }
    throw new Error("the page asked for " + url + ", which this DOM knows nothing about");
  }

  const kept = { "saneha.name": "surdy", "saneha.pins": JSON.stringify(options.pins || []) };
  const sandbox = {
    document,
    window: { addEventListener() {} },
    location: { pathname: options.at ? "/c/" + options.at : "/", reload() {} },
    history: { pushState() {} },
    localStorage: {
      getItem: (key) => (key in kept ? kept[key] : null),
      setItem: (key, value) => { kept[key] = value; }
    },
    fetch,
    AbortController,
    confirm: () => true,
    console,
    CSS: { escape: (text) => text },
    navigator: {},
    crypto,
    setTimeout,
    clearTimeout,
    setInterval: () => 0,
    clearInterval() {}
  };
  sandbox.window.document = document;

  return {
    sandbox,
    kept,
    requests,
    el: byId,
    rail: () => byId("channelList").innerHTML,
    async settle() {
      for (let turn = 0; turn < 20; turn += 1) {
        await new Promise((done) => setTimeout(done, 0));
      }
    }
  };
}

async function run(options) {
  const it = world(options);
  vm.createContext(it.sandbox);
  vm.runInContext(script, it.sandbox);
  await it.settle();
  return it;
}

/// The rows of the rail, in order, each as its name and the classes on it.
function rows(html) {
  const found = [];
  const pattern = /<div class="(ch[^"]*)" data-channel="([^"]+)"/g;
  let match;
  while ((match = pattern.exec(html))) found.push({ name: match[2], classes: match[1].split(" ") });
  return found;
}

/// The group headings, in order.
function groups(html) {
  return [...html.matchAll(/<div class="grp">.*?<span>([^<]+)<\/span>/g)].map((match) => match[1]);
}

(async () => {
  // ---- every channel is a hash and a name -------------------------------
  {
    const it = await run({});
    const html = it.rail();
    assert.ok(
      html.includes('<span class="hs">#</span><span class="cc">'),
      "an open channel is drawn as a hash and then its name"
    );
    assert.ok(!/padlock|#lk/.test(html), "the padlock is gone from the rail");
    assert.ok(
      html.includes('href="#sk"'),
      "a closed channel carries the struck hash"
    );
  }

  // ---- unread is weight and a badge, and only for open channels ---------
  {
    const it = await run({});
    const by = new Map(rows(it.rail()).map((row) => [row.name, row.classes]));
    assert.ok(by.get("ops").includes("un"), "a channel with unread messages is marked unread");
    assert.ok(by.get("brisk-otter").includes("un"), "and so is the other one");
    assert.ok(!by.get("xlaptop-1").includes("un"), "a channel read to the end is not");
    assert.ok(!by.get("madari-relay").includes("un"), "and neither is an empty one");
    assert.ok(
      by.get("notes-method").includes("cl") && !by.get("notes-method").includes("un"),
      "a closed channel is closed and never unread: there is nothing more coming"
    );
    assert.ok(it.rail().includes('<span class="bdg">12</span>'), "the count is what is behind");
  }

  // ---- with no pins, the order is the one it always was ------------------
  {
    const it = await run({});
    assert.deepStrictEqual(
      rows(it.rail()).map((row) => row.name),
      ["brisk-otter", "deploy-quadhost", "madari-relay", "ops", "xlaptop-1", "notes-method"],
      "open first and then by name, with the closed ones under their heading"
    );
    assert.deepStrictEqual(groups(it.rail()), ["Closed"], "and no headings above them");
  }

  // ---- pinned channels come first, in the order they were pinned --------
  {
    const it = await run({ pins: ["ops", "xlaptop-1"] });
    assert.deepStrictEqual(
      rows(it.rail()).map((row) => row.name),
      ["ops", "xlaptop-1", "brisk-otter", "deploy-quadhost", "madari-relay", "notes-method"],
      "the pinned two are first, in their kept order and not sorted"
    );
    assert.deepStrictEqual(groups(it.rail()), ["Pinned", "Open", "Closed"]);
  }

  // ---- a pin naming a channel that is gone holds no place ---------------
  {
    const it = await run({ pins: ["deleted-elsewhere", "ops"] });
    assert.deepStrictEqual(
      rows(it.rail()).map((row) => row.name)[0],
      "ops",
      "a pin on a channel that no longer exists is skipped rather than drawn"
    );
    assert.ok(!it.rail().includes("deleted-elsewhere"));
  }

  // ---- storage that is not a list of names leaves the rail unpinned -----
  {
    const it = await run({});
    it.kept["saneha.pins"] = "{not json";
    const again = await run({});
    assert.deepStrictEqual(groups(again.rail()), ["Closed"], "nothing is pinned and nothing is broken");
  }

  // ---- the head of an open channel ---------------------------------------
  {
    const it = await run({ at: "xlaptop-1" });
    assert.ok(
      it.el("channelName").innerHTML.includes('<span class="hs">#</span>xlaptop-1'),
      "the head is a hash and the name too"
    );
    assert.strictEqual(it.el("channelPurpose").textContent, "cross-laptop wake test");
    assert.strictEqual(it.el("channelPurpose").disabled, false, "an open channel's purpose is editable");
  }

  // ---- a channel with no purpose asks for one ---------------------------
  {
    const it = await run({ at: "madari-relay" });
    assert.strictEqual(
      it.el("channelPurpose").textContent,
      "say what this channel is for",
      "an empty purpose invites the line rather than leaving a gap"
    );
    assert.ok(it.el("channelPurpose").classes.has("none"));
  }

  // ---- a closed channel keeps the purpose it had, uneditable ------------
  {
    const it = await run({ at: "notes-method" });
    assert.strictEqual(it.el("channelPurpose").textContent, "how the vault gets written");
    assert.strictEqual(
      it.el("channelPurpose").disabled,
      true,
      "a closed channel is the record of a conversation that is over"
    );
    assert.strictEqual(it.el("purposeEditBtn").hidden, true);
  }

  // ---- editing the purpose sends one PATCH, as this person --------------
  {
    const it = await run({ at: "xlaptop-1" });
    it.el("channelPurpose").onclick();
    assert.strictEqual(it.el("purposeEdit").hidden, false, "the line becomes an input");
    assert.strictEqual(it.el("purposeInput").value, "cross-laptop wake test", "carrying what was there");

    it.el("purposeInput").value = "cross-laptop wake test, rung one";
    it.el("purposeInput").fire("keydown", { key: "Enter", preventDefault() {} });
    await it.settle();

    const patches = it.requests.filter((request) => request.init && request.init.method === "PATCH");
    assert.strictEqual(patches.length, 1, "one PATCH, and only one");
    assert.strictEqual(patches[0].url, "/channels/xlaptop-1");
    assert.deepStrictEqual(JSON.parse(patches[0].init.body), {
      by: ME,
      purpose: "cross-laptop wake test, rung one"
    });
    assert.strictEqual(it.el("purposeEdit").hidden, true, "and the input goes away again");
    assert.ok(
      it.rail().includes("cross-laptop wake test, rung one"),
      "the rail shows the new line without waiting for the next listing"
    );
  }

  // ---- a purpose that did not change is not sent ------------------------
  {
    const it = await run({ at: "xlaptop-1" });
    it.el("channelPurpose").onclick();
    it.el("purposeInput").fire("keydown", { key: "Enter", preventDefault() {} });
    await it.settle();
    assert.strictEqual(
      it.requests.filter((request) => request.init && request.init.method === "PATCH").length,
      0,
      "enter over an untouched line asks the server nothing"
    );
  }

  // ---- escape puts the line back, unchanged -----------------------------
  {
    const it = await run({ at: "xlaptop-1" });
    it.el("channelPurpose").onclick();
    it.el("purposeInput").value = "something else";
    it.el("purposeInput").fire("keydown", { key: "Escape", preventDefault() {} });
    await it.settle();
    assert.strictEqual(
      it.requests.filter((request) => request.init && request.init.method === "PATCH").length,
      0
    );
    assert.strictEqual(it.el("channelPurpose").textContent, "cross-laptop wake test");
  }

  // ---- the palette filters what the page is already holding -------------
  {
    const it = await run({});
    it.el("jump").onclick();
    assert.strictEqual(it.el("palette").hidden, false);

    it.el("paletteInput").value = "ot";
    it.el("paletteInput").fire("input");
    const listed = [...it.el("paletteList").innerHTML.matchAll(/data-open="([^"]+)"/g)].map((m) => m[1]);
    assert.deepStrictEqual(listed, ["brisk-otter", "notes-method"], "on the name, closed ones included");
    assert.ok(
      it.el("paletteList").innerHTML.includes("<mark>ot</mark>"),
      "with the run that matched marked in it"
    );

    it.el("paletteInput").value = "vault";
    it.el("paletteInput").fire("input");
    assert.deepStrictEqual(
      [...it.el("paletteList").innerHTML.matchAll(/data-open="([^"]+)"/g)].map((m) => m[1]),
      ["notes-method"],
      "and on the purpose, which is how a minted name is found"
    );

    it.el("paletteInput").value = "nothing matches this";
    it.el("paletteInput").fire("input");
    assert.ok(it.el("paletteList").innerHTML.includes("No channel matches that"));

    // Nothing was asked of the server for any of it.
    assert.strictEqual(
      it.requests.filter((request) => /search|filter/.test(request.url)).length,
      0,
      "the palette is a filter over what is in hand, not a question for the server"
    );
  }

  console.log("the rail, the palette and the purpose line hold");
})().catch((failure) => {
  console.error(failure);
  process.exit(1);
});
