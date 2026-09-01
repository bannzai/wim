// The two ways a file reaches the demo's buffer, driven the way someone would: a daemon this run
// starts over a directory of its own, and the browser's own file picker.
//
// The daemon half goes all the way to the disk — the file the browser saved is read back here —
// which is what makes it the machine check of "a file can be opened, edited and saved from the
// demo URL". The picker half cannot: the picker is the operating system's window and no page may
// open it on its own, so a stand-in stands where it would be and the run checks everything on
// this side of it. Opening the real picker stays a manual check.

import { spawn } from "node:child_process";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { expect, test } from "@playwright/test";

/**
 * The daemon binary this run talks to, which `make e2e` and the CI job build before the run.
 * `WIM_BINARY` names another build of it, such as a release one.
 */
const BINARY =
  process.env.WIM_BINARY ?? fileURLToPath(new URL("../../target/debug/wim", import.meta.url));

/**
 * How long the daemon has to print the address and the token it took.
 *
 * It prints them before it serves anything, so this covers process startup alone; ten seconds is
 * far longer than that takes on a loaded CI runner, and short enough that a binary which never
 * reports fails the run rather than sitting until Playwright's own timeout.
 */
const STARTUP_TIMEOUT = 10_000;

/** Port 0 so that the operating system picks a free one, and every parallel worker gets its own. */
const DAEMON_ADDR = "127.0.0.1:0";

/**
 * Starts a daemon over `root`, and answers with how to reach it once it says so.
 *
 * The address and the token are the two lines `wim serve` prints on startup, which is the only
 * place the token is disclosed.
 */
function startDaemon(root) {
  const daemon = spawn(BINARY, ["serve", "--addr", DAEMON_ADDR, "--root", root]);
  return new Promise((resolveDaemon, reject) => {
    let output = "";
    let errors = "";
    const fail = (reason) => {
      daemon.kill();
      reject(new Error(`${reason}\n${output}${errors}`));
    };
    const timer = setTimeout(
      () => fail(`${BINARY} did not report how to reach it within ${STARTUP_TIMEOUT}ms`),
      STARTUP_TIMEOUT,
    );
    daemon.on("error", (error) => {
      clearTimeout(timer);
      reject(new Error(`${BINARY} did not start: ${error.message}`));
    });
    daemon.stderr.setEncoding("utf8");
    daemon.stderr.on("data", (chunk) => {
      errors += chunk;
    });
    daemon.stdout.setEncoding("utf8");
    daemon.stdout.on("data", (chunk) => {
      output += chunk;
      const address = output.match(/^listening on (.+)$/m);
      const token = output.match(/^token: (.+)$/m);
      if (address === null || token === null) {
        return;
      }
      clearTimeout(timer);
      resolveDaemon({
        address: address[1],
        token: token[1],
        stop: () => daemon.kill(),
      });
    });
  });
}

/** The demo, once the editor behind it exists. */
async function openDemo(page) {
  await page.goto("/index.html");
  await page.waitForFunction(() => window.wimDemo !== undefined);
}

/** The line the demo reports opening and saving on. */
function statusOf(page) {
  return page.locator("#file-status");
}

/** Fills the daemon form with `path` on `daemon` and submits it. */
async function openThroughDaemon(page, daemon, path) {
  await page.fill("#daemon-address", daemon.address);
  await page.fill("#daemon-token", daemon.token);
  await page.fill("#daemon-path", path);
  await page.click("#daemon-form button[type=submit]");
}

/** Types `wim ` in front of the first line, which is an edit the file on disk does not have. */
async function editFirstLine(page) {
  await page.keyboard.press("i");
  await page.keyboard.type("wim ");
  await page.keyboard.press("Escape");
}

/** Runs `:w`, with `argument` when the command is to name where it writes. */
async function write(page, argument = "") {
  await page.keyboard.type(`:w${argument === "" ? "" : ` ${argument}`}`);
  await page.keyboard.press("Enter");
}

/**
 * Stands in for the file picker with a file the page holds, so that the run can drive the path a
 * picked file takes without the operating system's window.
 *
 * The handle is what the File System Access API hands over: a name, the file to read, and a
 * writable stream to put the buffer back through. What the demo wrote is left in `window.wimFile`
 * for the test to read.
 */
async function stubPicker(page, name, text) {
  await page.addInitScript(
    ([pickedName, pickedText]) => {
      window.wimFile = { name: pickedName, text: pickedText, written: null };
      window.showOpenFilePicker = async () => [
        {
          name: pickedName,
          getFile: async () => new File([window.wimFile.text], pickedName),
          createWritable: async () => ({
            write: async (data) => {
              window.wimFile.written = data;
            },
            close: async () => {},
          }),
        },
      ];
    },
    [name, text],
  );
}

test.describe("through a daemon", () => {
  /** The directory the daemon serves, which is where the files these tests read and write are. */
  let root;
  let daemon;

  test.beforeAll(async () => {
    root = await mkdtemp(join(tmpdir(), "wim-e2e-"));
    daemon = await startDaemon(root);
  });

  test.afterAll(() => {
    daemon?.stop();
  });

  test("opens a file, edits it and saves it back to disk", async ({ page }) => {
    await writeFile(join(root, "notes.md"), "hello\n");
    await openDemo(page);

    await openThroughDaemon(page, daemon, "notes.md");

    await expect(statusOf(page)).toHaveText("notes.md を開きました");
    expect(await page.evaluate(() => window.wimDemo.state().lines[0])).toBe("hello");

    await editFirstLine(page);
    await write(page);

    await expect(statusOf(page)).toHaveText("notes.md を保存しました");
    expect(await readFile(join(root, "notes.md"), "utf8")).toBe("wim hello\n");
  });

  test("writes to the path :w names rather than the one that was opened", async ({ page }) => {
    await writeFile(join(root, "source.md"), "hello\n");
    await openDemo(page);
    await openThroughDaemon(page, daemon, "source.md");
    await expect(statusOf(page)).toHaveText("source.md を開きました");

    await editFirstLine(page);
    await write(page, "copy.md");

    await expect(statusOf(page)).toHaveText("copy.md を保存しました");
    expect(await readFile(join(root, "copy.md"), "utf8")).toBe("wim hello\n");
    expect(await readFile(join(root, "source.md"), "utf8")).toBe("hello\n");
  });

  test("reports a token the daemon does not recognise", async ({ page }) => {
    await writeFile(join(root, "guarded.md"), "hello\n");
    await openDemo(page);

    await page.fill("#daemon-address", daemon.address);
    await page.fill("#daemon-token", "0".repeat(daemon.token.length));
    await page.fill("#daemon-path", "guarded.md");
    await page.click("#daemon-form button[type=submit]");

    await expect(statusOf(page)).toContainText("接続できません");
    expect(await page.evaluate(() => window.wimDemo.state().lines[0])).not.toBe("hello");
  });

  test("reports a path the daemon refuses to read", async ({ page }) => {
    await openDemo(page);

    await openThroughDaemon(page, daemon, "../outside.md");

    // The daemon serves one directory and nothing above it, so the path never becomes a buffer.
    await expect(statusOf(page)).toContainText("開けません");
  });

  test("leaves the keys typed into the file controls to them", async ({ page }) => {
    await openDemo(page);
    const before = await page.evaluate(() => window.wimDemo.state().text);

    await page.click("#daemon-address");
    await page.keyboard.type("iox");

    expect(await page.inputValue("#daemon-address")).toBe("iox");
    expect(await page.evaluate(() => window.wimDemo.state().text)).toBe(before);
    expect(await page.evaluate(() => window.wimDemo.state().mode)).toBe("NORMAL");
  });
});

test.describe("through the browser's own picker", () => {
  test("opens the picked file, edits it and writes it back through the handle", async ({
    page,
  }) => {
    await stubPicker(page, "local.md", "hello\n");
    await openDemo(page);

    await page.click("#local-open");

    await expect(statusOf(page)).toHaveText("local.md を開きました");
    expect(await page.evaluate(() => window.wimDemo.state().lines[0])).toBe("hello");

    await editFirstLine(page);
    await write(page);

    await expect(statusOf(page)).toHaveText("local.md を保存しました");
    expect(await page.evaluate(() => window.wimFile.written)).toBe("wim hello\n");
  });

  test("refuses a :w that names a path, which the browser gives no way to reach", async ({
    page,
  }) => {
    await stubPicker(page, "local.md", "hello\n");
    await openDemo(page);
    await page.click("#local-open");
    await expect(statusOf(page)).toHaveText("local.md を開きました");

    await editFirstLine(page);
    await write(page, "elsewhere.md");

    await expect(statusOf(page)).toHaveText("ローカルファイルでは :w にパスを指定できません");
    expect(await page.evaluate(() => window.wimFile.written)).toBeNull();
  });

  test("is offered only by a browser that has the API", async ({ page }) => {
    await page.addInitScript(() => {
      delete window.showOpenFilePicker;
    });
    await openDemo(page);

    await expect(page.locator("#local-open")).toBeDisabled();
  });
});
