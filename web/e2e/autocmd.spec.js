// The browser half of the autocmd acceptance check: the demo reads `web/wim.jsonc`, the core
// reports an event, and the handlers that config declares run.
//
// The native half runs the same config format over the same events through `wim edit`
// (`crates/wim/tests/autocmd.rs`), and the plugin handler here reaches the very component that
// one loads: the demo was given a jco transpile of it. What the two hosts have to agree on is
// the config that is accepted, the events that are raised and the plugin that is called, so this
// checks all three from the browser's side.

import { expect, test } from "@playwright/test";

/** The buffer the handlers run over, with the trailing blanks the config's `:s` takes away. */
const BUFFER = "alpha   \nbravo\n";

/** Opens the demo and waits for the plugins and the config to be in. */
async function open(page) {
  await page.goto("/index.html");
  await page.waitForFunction(() => window.wimDemo !== undefined);
  await page.evaluate(() => window.wimDemo.plugins());
  return page.evaluate(() => window.wimDemo.autocmds());
}

/**
 * Answers the demo's fetch of `wim.jsonc` with `body`, or with `status` alone when there is no
 * config to hand over, so that a run can drive a config the repository does not hold.
 *
 * It is served as the text the real one is served as (`web/serve.mjs`): JSONC is not JSON, and
 * nothing on the way is to read it as the JSON it is not.
 */
function serveConfig(page, { body = "", status = 200 } = {}) {
  return page.route("**/wim.jsonc", (route) =>
    route.fulfill({ status, contentType: "text/plain; charset=utf-8", body }),
  );
}

/**
 * Stands in for the file picker with a file the page holds, so that a run has somewhere for `:w`
 * to write without the operating system's window (`web/e2e/file-access.spec.js` drives the same
 * stand-in for the file access it checks).
 *
 * Every write that lands is left in `window.wimFile.writes`, in the order it landed, which is
 * what tells a save that happened once from one that happened without end.
 *
 * `gateFirstWrite` holds the first write until the test releases it with
 * `window.wimReleaseWrite()`, which is how a save is left in the air while something else happens.
 */
function stubPicker(page, name, text, { gateFirstWrite = false } = {}) {
  return page.addInitScript(
    ([pickedName, pickedText, holdFirstWrite]) => {
      window.wimFile = { writes: [] };
      let release = () => {};
      const held = holdFirstWrite ? new Promise((resolve) => (release = resolve)) : null;
      window.wimReleaseWrite = () => release();
      let writables = 0;
      window.showOpenFilePicker = async () => [
        {
          name: pickedName,
          getFile: async () => new File([pickedText], pickedName),
          createWritable: async () => {
            const wait = writables === 0 && held !== null ? held : Promise.resolve();
            writables += 1;
            let pending = null;
            return {
              write: async (data) => {
                pending = data;
              },
              close: async () => {
                await wait;
                window.wimFile.writes.push(pending);
              },
            };
          },
        },
      ];
    },
    [name, text, gateFirstWrite],
  );
}

/**
 * A plugin module of the shape jco writes, standing in for a transpiled component so that a run
 * can drive an answer the built sample plugin never gives.
 *
 * `edit` is what it answers every command and every event with, written as the ABI's `edit`
 * variant (`wit/plugin.wit`). The manifest and the module are both served by the route, so
 * nothing of the real transpile step is needed.
 */
function stubPlugin(page, name, edit) {
  const module = `
    const EDIT = ${JSON.stringify(edit)};
    const commands = { listCommands: () => [], run: () => EDIT };
    const events = { subscriptions: () => ["buffer-write"], onEvent: () => EDIT };
    export { commands as "wim:plugin/commands@0.1.0", events as "wim:plugin/events@0.1.0" };
  `;
  return Promise.all([
    page.route("**/plugins/manifest.json", (route) =>
      route.fulfill({
        status: 200,
        contentType: "application/json; charset=utf-8",
        body: JSON.stringify({
          abi: "0.1.0",
          plugins: [{ name, module: `./plugins/${name}.js` }],
        }),
      }),
    ),
    page.route(`**/plugins/${name}.js`, (route) =>
      route.fulfill({ status: 200, contentType: "text/javascript; charset=utf-8", body: module }),
    ),
  ]);
}

test("the autocmds wim.jsonc declares are the ones the demo registers", async ({ page }) => {
  const { declared, list } = await open(page);
  expect(declared).toEqual([
    { event: "buffer-write", handler: { kind: "ex", command: "%s/\\s+$//" } },
    { event: "buffer-write", handler: { kind: "plugin", plugin: "hello-wim" } },
  ]);
  await expect(page.locator("#autocmd-list")).toHaveText(list);
  expect(list).toBe("autocmd buffer-write → :%s/\\s+$// / buffer-write → hello-wim");
});

test("a write runs the handlers bound to it, in front of the write itself", async ({ page }) => {
  const { status } = await open(page);
  expect(status).toBe("autocmd はまだ実行していません");
  await page.evaluate((text) => window.wimDemo.load(text), BUFFER);
  await page.evaluate(() => window.wimDemo.sendKeys(":w"));
  const state = await page
    .evaluate(() => window.wimDemo.sendKeys("<CR>"))
    .then(() => page.evaluate(() => window.wimDemo.state()));
  // The buffer as it stands when the write is asked for is what a `:w` would put on the file,
  // and the trailing blanks are gone from it: the handler ran in front of the save.
  expect(state.text).toBe("alpha\nbravo\n");
  expect(state.autocmd.ran).toEqual([
    "buffer-write ex: %s/\\s+$//",
    "buffer-write plugin hello-wim: hello-wim saw `buffer-write` on [No Name]",
  ]);
  await expect(page.locator("#autocmd-status")).toHaveText(state.autocmd.status);
});

test("the event the core reports is the one the plugin is given", async ({ page }) => {
  await open(page);
  await page.evaluate((text) => window.wimDemo.load(text), BUFFER);
  const { effects } = await page.evaluate(() => window.wimDemo.sendKeys(":w<CR>"));
  // The event goes to the host in front of the save, which is the order the handlers run in.
  expect(effects.filter((effect) => effect.kind !== "event")).toEqual([
    { kind: "save", path: null },
  ]);
  expect(effects[effects.length - 1]).toEqual({
    kind: "event",
    name: "mode-changed",
    payload: '{"from":"COMMAND","to":"NORMAL"}',
  });
  expect(effects.some((effect) => effect.name === "buffer-write")).toBe(true);
});

test("a key that changes nothing runs no handler", async ({ page }) => {
  await open(page);
  await page.evaluate((text) => window.wimDemo.load(text), BUFFER);
  const state = await page
    .evaluate(() => window.wimDemo.sendKeys("j"))
    .then(() => page.evaluate(() => window.wimDemo.state()));
  expect(state.autocmd.ran).toEqual([]);
  expect(state.autocmd.status).toBe("autocmd はまだ実行していません");
  expect(state.text).toBe(BUFFER);
});

test("what a handler cannot do is reported rather than left silent", async ({ page }) => {
  await open(page);
  // Nothing to trim, so the `:s` of the config finds no match and says so where the handler that
  // ran it is reported. The plugin bound to the same event still runs.
  await page.evaluate(() => window.wimDemo.load("alpha\n"));
  const state = await page
    .evaluate(() => window.wimDemo.sendKeys(":w<CR>"))
    .then(() => page.evaluate(() => window.wimDemo.state()));
  expect(state.autocmd.ran[0]).toBe("buffer-write ex が失敗しました: pattern not found: \\s+$");
  expect(state.autocmd.ran[1]).toBe(
    "buffer-write plugin hello-wim: hello-wim saw `buffer-write` on [No Name]",
  );
  expect(state.text).toBe("alpha\n");
});

test("the config reader takes the dialect the native one takes, and no more", async ({ page }) => {
  await page.goto("/index.html");
  const answers = await page.evaluate(async () => {
    const { parseConfig } = await import("./config.js");
    const read = (text) => {
      try {
        return { autocmds: parseConfig(text) };
      } catch (error) {
        return { error: error.message };
      }
    };
    return {
      // Comments, a trailing comma, and a comment between the comma and the bracket.
      jsonc: read(`{
        // a line comment holding a " and a ,
        "autocmds": [
          /* and a block one */
          { "event": "text-changed", "handler": { "kind": "keys", "keys": "ggVGd" } },
          // the comma above trails
        ],
      }`),
      empty: read(""),
      unknownField: read('{"autocmd": []}'),
      unknownEvent: read(
        '{"autocmds":[{"event":"BufWritePre","handler":{"kind":"keys","keys":"x"}}]}',
      ),
      unknownKind: read('{"autocmds":[{"event":"text-changed","handler":{"kind":"vimscript"}}]}'),
      singleQuoted: read("{'autocmds': []}"),
      unclosedBlockComment: read("{} /*"),
      explicitNull: read("null"),
    };
  });
  expect(answers.jsonc.autocmds).toEqual([
    { event: "text-changed", handler: { kind: "keys", keys: "ggVGd" } },
  ]);
  expect(answers.empty.autocmds).toEqual([]);
  // Every one of these is refused natively as well (`crates/wim/src/config.rs`).
  expect(answers.unknownField.error).toContain("autocmd");
  expect(answers.unknownEvent.error).toContain("buffer-write");
  expect(answers.unknownKind.error).toContain("vimscript");
  expect(answers.singleQuoted.error).toBeDefined();
  // Reading the rest of the file as the comment would leave this accepted as `{}`, and the
  // native reader answers with `UnterminatedCommentBlock` rather than accepting it.
  expect(answers.unclosedBlockComment.error).toBeDefined();
  // An empty file is a config that binds nothing; the value `null` is a value the schema has no
  // place for. The native reader tells the two apart the same way (`crates/wim/src/config.rs`).
  expect(answers.explicitNull.error).toBeDefined();
});

test("a demo served without a config binds nothing and says so", async ({ page }) => {
  await serveConfig(page, { status: 404 });
  const { declared, list } = await open(page);
  expect(declared).toEqual([]);
  expect(list).toBe("autocmd は設定されていません");
});

test("a config the server cannot hand over is reported rather than read as none", async ({
  page,
}) => {
  // A 500 is a config that may well be there and was not read. Taking it for a demo served
  // without one would leave every handler the deployment declared silently unbound.
  await serveConfig(page, { status: 500 });
  const { declared, list } = await open(page);
  expect(declared).toEqual([]);
  expect(list).toContain("を読めません");
  expect(list).toContain("500");
});

test("a plugin binding that could never be delivered leaves nothing bound", async ({ page }) => {
  // hello-wim subscribes to `buffer-write` and to nothing else, so this binding could never
  // fire. The native host refuses the whole config over one of these before a key is typed
  // (`crates/wim/tests/autocmd.rs`), and the handler written beside it does not run here either.
  await serveConfig(page, {
    body: `{
      "autocmds": [
        { "event": "text-changed", "handler": { "kind": "ex", "command": "%s/a/b/" } },
        { "event": "text-changed", "handler": { "kind": "plugin", "plugin": "hello-wim" } }
      ]
    }`,
  });
  const { declared, list } = await open(page);
  expect(declared).toEqual([]);
  expect(list).toContain("配送されない autocmd があります");
  expect(list).toContain("hello-wim → text-changed");

  await page.evaluate(() => window.wimDemo.load("aaa\n"));
  const state = await page
    .evaluate(() => window.wimDemo.sendKeys("x"))
    .then(() => page.evaluate(() => window.wimDemo.state()));
  expect(state.autocmd.ran).toEqual([]);
  expect(state.text).toBe("aa\n");
});

test("a post-write handler that writes settles rather than writing without end", async ({
  page,
}) => {
  // The write a handler asks for lands after the handler has finished, and raising
  // `buffer-write-post` for it would run this handler again, and again for the write that one
  // asks for. Autocmds here never nest, and the native host settles the same way
  // (`crates/wim/tests/autocmd.rs`).
  await serveConfig(page, {
    body: `{
      "autocmds": [
        { "event": "buffer-write-post", "handler": { "kind": "keys", "keys": "i!<Esc>:w<CR>" } }
      ]
    }`,
  });
  await stubPicker(page, "post.md", "hello\n");
  await open(page);

  await page.click("#local-open");
  await expect(page.locator("#file-status")).toHaveText("post.md を開きました");

  await page.evaluate(() => window.wimDemo.sendKeys(":w<CR>"));

  // Two writes: the one `:w` asked for, and the one the handler it ran asked for. The second
  // raises no post event of its own, so that is where it stops.
  await expect.poll(() => page.evaluate(() => window.wimFile.writes)).toEqual([
    "hello\n",
    "!hello\n",
  ]);
  await page.waitForTimeout(250);
  expect(await page.evaluate(() => window.wimFile.writes.length)).toBe(2);
  expect(await page.evaluate(() => window.wimDemo.state().autocmd.ran)).toEqual([
    "buffer-write-post keys: i!<Esc>:w<CR>",
  ]);
});

test("a write that lands after another buffer was opened raises no post event", async ({
  page,
}) => {
  // The write is held while a sample is loaded over the buffer it was of. Raising
  // `buffer-write-post` once it lands would run a handler written for the file that was saved
  // over the buffer that is open now, which is another file's text entirely.
  await serveConfig(page, {
    body: `{
      "autocmds": [
        { "event": "buffer-write-post", "handler": { "kind": "ex", "command": "%s/^/POST /" } }
      ]
    }`,
  });
  await stubPicker(page, "held.md", "hello\n", { gateFirstWrite: true });
  await open(page);

  await page.click("#local-open");
  await expect(page.locator("#file-status")).toHaveText("held.md を開きました");
  await page.evaluate(() => window.wimDemo.sendKeys(":w<CR>"));
  expect(await page.evaluate(() => window.wimFile.writes)).toEqual([]);

  // A sample is a buffer of its own, and loading one puts a new editor in place of the saved one.
  await page.click("#sample-rust");
  await expect(page.locator("#file-status")).toHaveText("sample.rs を読み込みました");

  await page.evaluate(() => window.wimReleaseWrite());
  await expect(page.locator("#file-status")).toHaveText("held.md を保存しました");

  const state = await page.evaluate(() => window.wimDemo.state());
  expect(state.lines[0]).toBe("// Rust, highlighted by tree-sitter.");
  expect(state.autocmd.ran).toEqual([]);
});

test("an edit naming lines the buffer does not have fails its handler", async ({ page }) => {
  // `plugin::apply` refuses the same edit natively and the handler is reported as failed
  // (`crates/wim/src/plugin.rs`); answering with the complaint as a message would have it
  // reported as one that ran.
  await stubPlugin(page, "out-of-range", {
    tag: "replace-lines",
    val: { start: 40, end: 50, text: "" },
  });
  await serveConfig(page, {
    body: `{
      "autocmds": [
        { "event": "buffer-write", "handler": { "kind": "plugin", "plugin": "out-of-range" } }
      ]
    }`,
  });
  await open(page);

  const before = await page.evaluate(() => window.wimDemo.state().text);
  const state = await page
    .evaluate(() => window.wimDemo.sendKeys(":w<CR>"))
    .then(() => page.evaluate(() => window.wimDemo.state()));

  expect(state.autocmd.ran).toHaveLength(1);
  expect(state.autocmd.ran[0]).toContain("plugin が失敗しました");
  expect(state.autocmd.ran[0]).toContain("行のバッファにありません");
  expect(state.text).toBe(before);
});

test("a key pressed while the config is in the air is typed once it lands", async ({ page }) => {
  let landConfig;
  const held = new Promise((resolve) => {
    landConfig = resolve;
  });
  await page.route("**/wim.jsonc", async (route) => {
    await held;
    await route.fulfill({
      status: 200,
      contentType: "text/plain; charset=utf-8",
      body: `{"autocmds": [{ "event": "text-changed",
                            "handler": { "kind": "ex", "command": "%s/is/IS/" } }]}`,
    });
  });
  // `commit` rather than `load`: the demo's script waits for the config, and a goto waiting for
  // that script to finish would wait for the very thing this run is holding.
  await page.goto("/index.html", { waitUntil: "commit" });
  await page.waitForFunction(() => window.wimDemo !== undefined);

  const before = await page.evaluate(() => window.wimDemo.state().text);
  await page.keyboard.press("x");
  expect(await page.evaluate(() => window.wimDemo.state().text)).toBe(before);

  landConfig();
  const state = await page
    .evaluate(() => window.wimDemo.autocmds())
    .then(() => page.evaluate(() => window.wimDemo.state()));

  // The key was held rather than dropped, and the `text-changed` it raised found the handler
  // bound: an event is reported once, so one raised over an empty config is gone for good.
  expect(state.text).not.toBe(before);
  expect(state.autocmd.ran).toEqual(["text-changed ex: %s/is/IS/"]);
});
