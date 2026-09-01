// What the browser host makes of the plugins a manifest names, and when it runs their commands,
// over stand-in modules rather than transpiled components.
//
// What these two things have in common is that neither can be set up with the plugins the build
// produces: two components publishing one name is not something `make build-plugins` makes, and
// nor is one whose command refuses every line. So the modules are written here. They are held to
// the same ABI the real ones are, and what the host reads off them — `list-commands`,
// `subscriptions`, `render`, `run` — is all this needs.

import { expect, test } from "@playwright/test";

/** The ABI the stand-in modules are written against, which is the one the manifest declares. */
const ABI = "0.1.0";

/**
 * A module exporting the world's three interfaces at `ABI`, publishing `commandNames` and
 * subscribing to `events`.
 *
 * The export names are the ones jco writes for a transpiled component, which is what the host
 * looks a plugin up by (`web/plugins.js`).
 */
function stub(commandNames, events = [], refuses = false) {
  const published = commandNames.map((name) => ({ name, description: `${name} のスタブ` }));
  return `
const commands = {
  listCommands: () => ${JSON.stringify(published)},
  // What a plugin refuses arrives as the error half of the ABI's \`result<edit, string>\`, which
  // is a throw on this side of a jco transpile (\`web/plugins.js\`).
  run: (name) => {
    if (${JSON.stringify(refuses)}) {
      throw new Error(name + " は断りました");
    }
    return { tag: "message", val: name };
  },
};
const events = {
  subscriptions: () => ${JSON.stringify(events)},
  onEvent: () => ({ tag: "noop" }),
};
const ui = { render: () => undefined };
export {
  commands as "wim:plugin/commands@${ABI}",
  events as "wim:plugin/events@${ABI}",
  ui as "wim:plugin/ui@${ABI}",
};
`;
}

/**
 * Serves `declared` as the manifest the host fetches, with a stand-in module behind each entry.
 *
 * Routed rather than written to disk: the demo the run serves is the one `make build-web-plugins`
 * left, and these plugins exist for the length of one test.
 */
async function serveStubs(page, declared) {
  await page.route("**/plugins/manifest.json", (route) =>
    route.fulfill({
      contentType: "application/json; charset=utf-8",
      body: JSON.stringify({
        abi: ABI,
        plugins: declared.map(({ name }) => ({ name, module: `./stub-${name}.js` })),
      }),
    }),
  );
  for (const { name, commands, events, refuses } of declared) {
    await page.route(`**/stub-${name}.js`, (route) =>
      route.fulfill({
        contentType: "text/javascript; charset=utf-8",
        body: stub(commands, events, refuses === true),
      }),
    );
  }
}

/** Serves `autocmds` as the config the demo reads, in place of the one the repo ships. */
async function serveConfig(page, autocmds) {
  await page.route("**/wim.jsonc", (route) =>
    route.fulfill({
      contentType: "text/plain; charset=utf-8",
      body: JSON.stringify({ autocmds }),
    }),
  );
}

/** Opens the demo and answers with the commands it registered and what startup reported. */
async function open(page) {
  await page.goto("/index.html");
  await page.waitForFunction(() => window.wimDemo !== undefined);
  return page.evaluate(() => window.wimDemo.plugins());
}

test("a command name two plugins publish is refused, and the report names both", async ({
  page,
}) => {
  // `hello-wim` is the plugin the demo's config binds a handler to, so it is the one that keeps
  // the name and `other` is the one turned away.
  await serveStubs(page, [
    { name: "hello-wim", commands: ["upcase"], events: ["buffer-write"] },
    { name: "other", commands: ["upcase", "downcase"] },
  ]);

  const { commands, status } = await open(page);

  expect(commands).toEqual([
    { name: "upcase", description: "upcase のスタブ", plugin: "hello-wim" },
  ]);
  expect(status).toContain(":upcase");
  expect(status).toContain("hello-wim");
  expect(status).toContain("other");
});

test("a plugin refused over one name registers none of its other commands", async ({ page }) => {
  await serveStubs(page, [
    { name: "hello-wim", commands: ["upcase"], events: ["buffer-write"] },
    { name: "other", commands: ["downcase", "upcase"] },
  ]);

  const { commands } = await open(page);

  expect(commands.map((command) => command.name)).toEqual(["upcase"]);
});

test("plugins that publish names of their own each keep theirs", async ({ page }) => {
  await serveStubs(page, [
    { name: "hello-wim", commands: ["upcase"], events: ["buffer-write"] },
    { name: "other", commands: ["downcase"] },
  ]);

  const { commands, status } = await open(page);

  expect(commands.map((command) => [command.name, command.plugin])).toEqual([
    ["upcase", "hello-wim"],
    ["downcase", "other"],
  ]);
  expect(status).not.toContain("読み込めないプラグインがあります");
});

test("a handler's command runs before the keys written behind it", async ({ page }) => {
  // The keys of the handler are a `:` line and an `x`. The line names a command that refuses, and
  // a handler ends at what it was refused with the way a native run does — so the `x` is never
  // typed and the buffer is the one the command was handed (`crates/wim/src/edit.rs`).
  await serveStubs(page, [{ name: "refuser", commands: ["refuse"], refuses: true }]);
  await serveConfig(page, [
    { event: "text-changed", handler: { kind: "keys", keys: ":refuse<CR>x" } },
  ]);
  await page.goto("/index.html");
  await page.waitForFunction(() => window.wimDemo !== undefined);
  await page.evaluate(() => window.wimDemo.plugins());
  await page.evaluate(() => window.wimDemo.load("alpha\n"));

  await page.evaluate(() => window.wimDemo.sendKeys("x"));

  const state = await page.evaluate(() => window.wimDemo.state());
  expect(state.text).toBe("lpha\n");
  const { status } = await page.evaluate(() => window.wimDemo.autocmds());
  expect(status).toContain("refuse");
  expect(state.plugin.status).toContain("refuse");
});
