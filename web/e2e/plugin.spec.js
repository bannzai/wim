// The acceptance check of the plugin ABI: one `.wasm` component, two hosts, the same answers.
//
// The component this drives is the one `make build-plugins` built. The browser was given a jco
// transpile of it and the native binary loads the file itself through wasmtime, so a run here
// puts the same input through both halves of `wit/plugin.wit` and compares what comes back. A
// difference is what a divergence between the two hosts looks like from the outside, whether it
// is in the ABI, in the transpile or in how an edit is applied.
//
// The component's path arrives as `WIM_PLUGIN_WASM`, the way it does for the native tests
// (`crates/wim/tests/plugin.rs`), and the run steps aside without it: a machine that cannot build
// a component has nothing to compare.

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { expect, test } from "@playwright/test";

/** The binary the native half runs, which `make e2e` and the CI job build before the run. */
const BINARY =
  process.env.WIM_BINARY ?? fileURLToPath(new URL("../../target/debug/wim", import.meta.url));

/** The component both hosts load. Unset on a machine that cannot build one. */
const WASM = process.env.WIM_PLUGIN_WASM;

/** The buffer the commands run over, in the shape a file has: a newline closing the last line. */
const BUFFER = "hello\nwim\n";

test.skip(
  WASM === undefined,
  "WIM_PLUGIN_WASM is not set, so there is no component for the two hosts to share",
);

// Set and naming nothing is a build that moved rather than a machine that cannot build: it would
// otherwise leave the acceptance check quietly skipped in the one run that exists to make it.
test.beforeAll(() => {
  expect(existsSync(WASM), `WIM_PLUGIN_WASM names ${WASM}, which is not a file`).toBe(true);
});

/**
 * What `wim plugin run` makes of `command` over `BUFFER`: the buffer it wrote out, or the message
 * it refused with.
 *
 * The subcommand hands the plugin a snapshot with no name and the cursor at the start, so the
 * browser half below runs over a buffer that came from no file to be given the same one.
 */
function native(command, args) {
  const run = spawnSync(BINARY, ["plugin", "run", WASM, command, ...args, "--input", BUFFER], {
    encoding: "utf8",
  });
  expect(run.error, `${BINARY} should run`).toBeUndefined();
  return run.status === 0
    ? { text: run.stdout }
    : // `wim: :name failed: ` is the binary's own framing; what the plugin said is the rest.
      { error: run.stderr.trim().replace(`wim: :${command} failed: `, "") };
}

/** Types `:command args` into the demo and answers with the state it left behind. */
async function browser(page, command, args) {
  await page.goto("/index.html");
  await page.waitForFunction(() => window.wimDemo !== undefined);
  await page.evaluate(() => window.wimDemo.plugins());
  // No name, so the plugin is given the same snapshot the subcommand hands it.
  await page.evaluate((text) => window.wimDemo.load(text), BUFFER);
  await page.evaluate((typed) => window.wimDemo.sendKeys(typed), [command, ...args].join(" "));
  await page.evaluate(() => window.wimDemo.sendKeys("<CR>"));
  return page.evaluate(() => window.wimDemo.state());
}

test("the commands the plugin publishes are registered from the browser", async ({ page }) => {
  await page.goto("/index.html");
  await page.waitForFunction(() => window.wimDemo !== undefined);
  const { commands, status } = await page.evaluate(() => window.wimDemo.plugins());
  expect(commands).toEqual([
    { name: "upcase", description: "Uppercases the whole buffer.", plugin: "hello-wim" },
  ]);
  // Nothing failed to load, which is the other thing the status line would be holding.
  expect(status).toBe("プラグインのコマンドはまだ実行していません");
  await expect(page.locator("#plugin-commands")).toHaveText(
    "プラグインのコマンド :upcase — Uppercases the whole buffer.",
  );
});

test("the buffer :upcase hands back is the one the native host writes", async ({ page }) => {
  const expected = native("upcase", []);
  expect(expected.text).toBe("HELLO\nWIM\n");
  const state = await browser(page, ":upcase", []);
  expect(state.text).toBe(expected.text);
  expect(state.plugin.status).toBe(":upcase: バッファを書き換えました");
});

test("a name no plugin published is still the core's to refuse", async ({ page }) => {
  // The host routes a line to a plugin only when the plugin registered that name, so the one
  // error hello-wim can raise on its own — `hello-wim has no command named ...`, which the native
  // subcommand reaches by naming a command directly — is unreachable through an Ex line, and what
  // answers instead is the core.
  const state = await browser(page, ":nope", []);
  expect(state.effects).toEqual([{ kind: "error", message: "not an editor command: nope" }]);
  expect(state.text).toBe(BUFFER);
});

test("an argument :upcase does not take is refused in the same words", async ({ page }) => {
  const expected = native("upcase", ["x"]);
  expect(expected.error).toBe(":upcase takes no arguments");
  const state = await browser(page, ":upcase", ["x"]);
  expect(state.plugin.status).toBe(`:upcase が失敗しました: ${expected.error}`);
  // What a plugin refuses leaves the buffer as it was, here as it does natively.
  expect(state.text).toBe(BUFFER);
});
