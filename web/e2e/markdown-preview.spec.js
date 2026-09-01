// The browser half of the Markdown Preview acceptance check: a plugin answers `ui.render` with a
// panel, the demo draws it, and nothing in it runs.
//
// Most of what is checked here is the host's, not the plugin's: which buffers get a panel, when
// the panel is drawn again, and where the HTML ends up. So most of these run against a stand-in
// module of the shape jco writes rather than against the built component — the same stand-in
// `web/e2e/autocmd.spec.js` drives an edit through — which is what lets them check answers the
// built plugin never gives and run on a machine that cannot build one. What the plugin itself
// makes of Markdown is pinned in its own tests (`plugins/markdown-preview/src/lib.rs`) and put
// through wasmtime by `crates/wim/tests/plugin.rs`.
//
// The last test is the one that needs the real thing: it runs the built component through both
// hosts and checks that the panel the demo shows is the HTML the native host writes, the way
// `web/e2e/plugin.spec.js` does for a command. It steps aside where there is no component.

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { expect, test } from "@playwright/test";

/** The binary the native half runs, which `make e2e` and the CI job build before the run. */
const BINARY =
  process.env.WIM_BINARY ?? fileURLToPath(new URL("../../target/debug/wim", import.meta.url));

/** The built Markdown Preview component. Unset on a machine that cannot build one. */
const WASM = process.env.WIM_MARKDOWN_PREVIEW_WASM;

/** The name the demo loads a panelled buffer under, which is what the plugin decides by. */
const MARKDOWN = "notes.md";

/** The selector of the panel a plugin of that name has open. */
function panelFrame(page, plugin = "markdown-preview") {
  return page.frameLocator(`.panel[data-plugin="${plugin}"] iframe`);
}

/**
 * A plugin module of the shape jco writes, standing in for the transpiled Markdown Preview.
 *
 * It answers the two ways the real one does — a panel for a `.md` buffer, `none` for anything
 * else — and renders enough of Markdown for a run to tell one rendering from the next: a `#`
 * heading, a `-` list item, a line of raw HTML passed through the way CommonMark passes it, and a
 * paragraph for everything else. Nothing here is the real reader, and nothing here is asserted
 * about as though it were; what it stands in for is the shape of the answer.
 */
function stubPreview(page, name = "markdown-preview") {
  const module = `
    function html(text) {
      return text
        .split("\\n")
        .filter((line) => line !== "")
        .map((line) => {
          if (line.startsWith("# ")) return "<h1>" + line.slice(2) + "</h1>";
          if (line.startsWith("- ")) return "<li>" + line.slice(2) + "</li>";
          // Raw HTML is passed through, which is what CommonMark says to do with it and why the
          // host does not trust what comes back (\`wit/README.md\`).
          if (line.startsWith("<")) return line;
          return "<p>" + line + "</p>";
        })
        .join("\\n");
    }
    const commands = { listCommands: () => [], run: () => { throw new Error("no commands"); } };
    const events = { subscriptions: () => [], onEvent: () => { throw new Error("no events"); } };
    const ui = {
      render: (buf) =>
        buf.name.endsWith(".md")
          ? { title: "Markdown Preview", html: html(buf.text) }
          : undefined,
    };
    export {
      commands as "wim:plugin/commands@0.1.0",
      events as "wim:plugin/events@0.1.0",
      ui as "wim:plugin/ui@0.1.0",
    };
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

/** Opens the demo and waits for the plugins and the config to be in. */
async function open(page) {
  await page.goto("/index.html");
  await page.waitForFunction(() => window.wimDemo !== undefined);
  await page.evaluate(() => window.wimDemo.plugins());
  await page.evaluate(() => window.wimDemo.autocmds());
}

/** The panels the demo has open, as the run reads them off the page. */
function panels(page) {
  return page.evaluate(() => window.wimDemo.state().plugin.panels);
}

test("a Markdown buffer opens a panel and every other buffer closes it", async ({ page }) => {
  await stubPreview(page);
  await open(page);

  // The demo starts on a buffer that came from no file, which has no name and no language.
  expect(await panels(page)).toEqual([]);
  await expect(page.locator("#panels .panel")).toHaveCount(0);

  await page.evaluate((name) => window.wimDemo.load("# Title\n", name), MARKDOWN);
  expect(await panels(page)).toHaveLength(1);
  expect((await panels(page))[0].plugin).toBe("markdown-preview");
  expect((await panels(page))[0].title).toBe("Markdown Preview");
  await expect(panelFrame(page).locator("h1")).toHaveText("Title");

  // A buffer of another language is not one this plugin has a panel for, and `none` is what
  // closes the one that is open (`wit/plugin.wit`).
  await page.evaluate(() => window.wimDemo.load("fn main() {}\n", "main.rs"));
  expect(await panels(page)).toEqual([]);
  await expect(page.locator("#panels .panel")).toHaveCount(0);
});

test("editing the Markdown redraws the panel", async ({ page }) => {
  await stubPreview(page);
  await open(page);
  await page.evaluate((name) => window.wimDemo.load("# Title\n", name), MARKDOWN);
  await expect(panelFrame(page).locator("li")).toHaveCount(0);

  // A closed change is what raises `text-changed`, which is one of the two things the host draws
  // the panel again for. An Insert session closes on `<Esc>`, the way Vim's TextChanged does
  // (`documents/CONFIG.md`).
  await page.evaluate(() => window.wimDemo.sendKeys("o- one<Esc>"));
  await expect(panelFrame(page).locator("h1")).toHaveText("Title");
  await expect(panelFrame(page).locator("li")).toHaveText("one");

  // And again for the next edit, over the buffer as it stands by then.
  await page.evaluate(() => window.wimDemo.sendKeys("o- two<Esc>"));
  await expect(panelFrame(page).locator("li")).toHaveText(["one", "two"]);

  // A key that changes nothing leaves the panel where it is rather than reloading the frame.
  const before = (await panels(page))[0].srcdoc;
  await page.evaluate(() => window.wimDemo.sendKeys("gg"));
  expect((await panels(page))[0].srcdoc).toBe(before);
});

test("nothing in a panel runs, and nothing in it reaches the page", async ({ page }) => {
  await stubPreview(page);
  await open(page);
  // Markdown passes raw HTML through, so this is what a buffer holding a script renders as. The
  // panel is drawn in a frame with an empty `sandbox`, where a script is markup and not code.
  await page.evaluate(
    (name) =>
      window.wimDemo.load(
        [
          "# Title",
          "",
          '<script>document.body.setAttribute("data-script-ran", "yes");' +
            'window.parent.wimEscaped = true;<\/script>',
          '<img src="data:image/png;base64,not-an-image" ' +
            "onerror=\"document.body.setAttribute('data-onerror-ran', 'yes')\">",
        ].join("\n") + "\n",
        name,
      ),
    MARKDOWN,
  );

  // The frame is sandboxed with nothing allowed, which is the whole of why the rest holds.
  await expect(page.locator('.panel[data-plugin="markdown-preview"] iframe')).toHaveAttribute(
    "sandbox",
    "",
  );
  // The markup arrived as it was written — it was not stripped on the way — and the heading
  // beside it rendered, so the panel really is holding this buffer.
  await expect(panelFrame(page).locator("h1")).toHaveText("Title");
  expect(await panelFrame(page).locator("script").count()).toBe(1);

  // An `onerror` needs the load to fail, and a frame that has settled is one where it has.
  await expect(panelFrame(page).locator("img")).toHaveCount(1);
  await page.waitForTimeout(250);
  const body = panelFrame(page).locator("body");
  await expect(body).not.toHaveAttribute("data-script-ran", "yes");
  await expect(body).not.toHaveAttribute("data-onerror-ran", "yes");
  // Nothing of the page it is drawn on was reachable either, which an opaque origin is what
  // makes so: `allow-same-origin` is not in the sandbox any more than `allow-scripts` is.
  expect(await page.evaluate(() => window.wimEscaped)).toBeUndefined();
});

test("a plugin whose panel fails is reported and loses its panel alone", async ({ page }) => {
  // A plugin that traps comes back to the browser host as a thrown error, the way a refusal
  // does. The panel it had is closed rather than left holding what it last said.
  await page.route("**/plugins/manifest.json", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json; charset=utf-8",
      body: JSON.stringify({
        abi: "0.1.0",
        plugins: [{ name: "broken-panel", module: "./plugins/broken-panel.js" }],
      }),
    }),
  );
  await page.route("**/plugins/broken-panel.js", (route) =>
    route.fulfill({
      status: 200,
      contentType: "text/javascript; charset=utf-8",
      body: `
        const commands = { listCommands: () => [], run: () => { throw new Error("no"); } };
        const events = { subscriptions: () => [], onEvent: () => { throw new Error("no"); } };
        const ui = { render: () => { throw new Error("the panel gave up"); } };
        export {
          commands as "wim:plugin/commands@0.1.0",
          events as "wim:plugin/events@0.1.0",
          ui as "wim:plugin/ui@0.1.0",
        };
      `,
    }),
  );
  await open(page);

  expect(await panels(page)).toEqual([]);
  await expect(page.locator("#plugin-status")).toContainText("the panel gave up");
  // The demo is still the demo: a panel that could not be drawn does not stop the editing.
  await page.evaluate(() => window.wimDemo.sendKeys("x"));
  expect(await page.evaluate(() => window.wimDemo.state().text)).not.toBe("");
});

test.describe("the built component", () => {
  test.skip(
    WASM === undefined,
    "WIM_MARKDOWN_PREVIEW_WASM is not set, so there is no component for the two hosts to share",
  );

  // Set and naming nothing is a build that moved rather than a machine that cannot build one.
  test.beforeAll(() => {
    expect(existsSync(WASM), `WIM_MARKDOWN_PREVIEW_WASM names ${WASM}, which is not a file`).toBe(
      true,
    );
  });

  test("the panel the demo draws is the HTML the native host renders", async ({ page }) => {
    await open(page);
    // The Markdown sample, which is a `.md` buffer and the one the demo can load without a file.
    await page.click("#sample-markdown");
    await expect(page.locator("#file-status")).toHaveText("sample.md を読み込みました");

    const text = await page.evaluate(() => window.wimDemo.state().text);
    const native = spawnSync(
      BINARY,
      ["plugin", "render", WASM, "--name", "sample.md", "--input", text],
      { encoding: "utf8" },
    );
    expect(native.error, `${BINARY} should run`).toBeUndefined();
    expect(native.status, native.stderr).toBe(0);
    expect(native.stderr).toContain("Markdown Preview");
    expect(native.stdout).not.toBe("");

    const panel = (await panels(page)).find((panel) => panel.plugin === "markdown-preview");
    expect(panel, "the Markdown sample should have a panel").toBeDefined();
    expect(panel.title).toBe("Markdown Preview");
    // The same string, put into the demo's own page: one component, two hosts, one answer.
    expect(panel.srcdoc).toContain(native.stdout);
    await expect(panelFrame(page).locator("h1")).toHaveText(
      "Markdown, highlighted by tree-sitter",
    );
  });

  test("a buffer that is not Markdown has no panel in either host", async ({ page }) => {
    await open(page);
    await page.click("#sample-rust");
    await expect(page.locator("#file-status")).toHaveText("sample.rs を読み込みました");

    const text = await page.evaluate(() => window.wimDemo.state().text);
    const native = spawnSync(
      BINARY,
      ["plugin", "render", WASM, "--name", "sample.rs", "--input", text],
      { encoding: "utf8" },
    );
    // `none` is an answer and not a failure, so the run succeeds with nothing written.
    expect(native.status, native.stderr).toBe(0);
    expect(native.stdout).toBe("");
    expect(native.stderr).toContain("no panel");

    expect((await panels(page)).map((panel) => panel.plugin)).not.toContain("markdown-preview");
  });
});
