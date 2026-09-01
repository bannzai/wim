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
});
