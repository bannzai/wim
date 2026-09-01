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

/** Types `text` where the cursor is and goes back to Normal mode. */
async function insert(page, text) {
  await page.keyboard.press("i");
  await page.keyboard.type(text);
  await page.keyboard.press("Escape");
}

/** Types `wim ` in front of the first line, which is an edit the file on disk does not have. */
function editFirstLine(page) {
  return insert(page, "wim ");
}

/** Opens a line under the cursor holding `text`, which is the edit that adds a line separator. */
async function openLineBelow(page, text) {
  await page.keyboard.press("o");
  await page.keyboard.type(text);
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
 * for the test to read: `written` is the text of the last write to have finished, and `writes`
 * holds every one of them in the order they landed on the file.
 *
 * `text` is what the file holds, as a string for a UTF-8 one and as the bytes themselves for a
 * file that is not.
 *
 * A writable puts its text on the file when it is closed rather than when it is written, which is
 * what the API's siloed streams do, so a run can hold one open and see what a second save does
 * while the first has not landed. `gateFirstWrite` holds the first one until the test releases it
 * with `window.wimReleaseWrite()`, and `gatePick` holds the pick itself until
 * `window.wimReleasePick()`, which is how an open is left in the air while another starts.
 */
async function stubPicker(page, name, text, { gateFirstWrite = false, gatePick = false } = {}) {
  await page.addInitScript(
    ([pickedName, pickedText, holdFirstWrite, holdPick]) => {
      window.wimFile = { name: pickedName, text: pickedText, written: null, writes: [] };
      const gate = (hold) => {
        if (!hold) {
          return { wait: Promise.resolve(), release: () => {} };
        }
        let release;
        return { wait: new Promise((resolve) => (release = resolve)), release: () => release() };
      };
      const pick = gate(holdPick);
      const firstWrite = gate(holdFirstWrite);
      window.wimReleasePick = pick.release;
      window.wimReleaseWrite = firstWrite.release;
      let writables = 0;
      window.showOpenFilePicker = async () => [
        {
          name: pickedName,
          getFile: async () => {
            await pick.wait;
            const content = window.wimFile.text;
            return new File(
              [typeof content === "string" ? content : new Uint8Array(content)],
              pickedName,
            );
          },
          createWritable: async () => {
            const held = writables === 0 ? firstWrite.wait : Promise.resolve();
            writables += 1;
            let pending = null;
            return {
              write: async (data) => {
                pending = data;
              },
              close: async () => {
                await held;
                window.wimFile.writes.push(pending);
                window.wimFile.written = pending;
              },
            };
          },
        },
      ];
    },
    [name, text, gateFirstWrite, gatePick],
  );
}

/**
 * Stands in for a picker that is closed without choosing anything, which is what the File System
 * Access API reports by rejecting with an `AbortError`.
 *
 * `window.wimPickerCalls` counts how many times it was opened, so that a run can tell a pick that
 * was canceled from one that never happened at all.
 */
async function stubCanceledPicker(page) {
  await page.addInitScript(() => {
    window.wimPickerCalls = 0;
    window.showOpenFilePicker = async () => {
      window.wimPickerCalls += 1;
      throw new DOMException("The user aborted a request.", "AbortError");
    };
  });
}

/**
 * Wraps the WebSocket the page opens so that a daemon exchange can be held where the run wants it.
 *
 * The daemon these tests talk to is a real one on the same machine and answers at once, which
 * leaves no window in which a second thing can happen while an open or a save is in the air.
 * Holding the exchange on this side of the wire opens that window without a slow daemon to stand
 * in for.
 *
 * `holdOpen` keeps every connection from reporting itself open until `window.wimReleaseConnect()`,
 * which is how an open is left connecting. `holdWrites` keeps `fs.write` requests from going out
 * until `window.wimReleaseDaemonWrites()`, which is how saves are left queued. Nothing else is
 * held either way, so a second file can still be connected to and read while the saves of the
 * first are waiting.
 */
async function gateDaemonSocket(page, { holdOpen = false, holdWrites = false } = {}) {
  await page.addInitScript(
    ([gateOpen, gateWrites]) => {
      const Native = window.WebSocket;
      let openReleased = !gateOpen;
      let writesReleased = !gateWrites;
      const heldOpens = [];
      const heldWrites = [];
      window.wimReleaseConnect = () => {
        openReleased = true;
        for (const announce of heldOpens.splice(0)) {
          announce();
        }
      };
      window.wimReleaseDaemonWrites = () => {
        writesReleased = true;
        for (const send of heldWrites.splice(0)) {
          send();
        }
      };
      window.WebSocket = class GatedSocket extends Native {
        addEventListener(type, listener, options) {
          if (type !== "open" || openReleased) {
            super.addEventListener(type, listener, options);
            return;
          }
          super.addEventListener(
            type,
            (event) => {
              if (openReleased) {
                listener(event);
                return;
              }
              heldOpens.push(() => listener(event));
            },
            options,
          );
        }
        send(data) {
          // The requests are wim-protocol's JSON, so the method a frame carries is in its text.
          if (writesReleased || typeof data !== "string" || !data.includes('"fs.write"')) {
            super.send(data);
            return;
          }
          heldWrites.push(() => super.send(data));
        }
      };
    },
    [holdOpen, holdWrites],
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

  test("saves a CRLF file back with the line endings it was opened with", async ({ page }) => {
    await writeFile(join(root, "crlf.md"), "hello\r\nworld\r\n");
    await openDemo(page);

    await openThroughDaemon(page, daemon, "crlf.md");

    await expect(statusOf(page)).toHaveText("crlf.md を開きました");
    // The core edits LF text, so the carriage returns are not in the buffer to be edited around.
    // A trailing newline terminates the last line rather than opening an empty one, so the
    // buffer holds the two lines the file has.
    expect(await page.evaluate(() => window.wimDemo.state().lines)).toEqual(["hello", "world"]);

    await openLineBelow(page, "added");
    await write(page);

    await expect(statusOf(page)).toHaveText("crlf.md を保存しました");
    // The line the edit added ends the way the ones the file came with do, rather than leaving
    // the file holding both endings.
    expect(await readFile(join(root, "crlf.md"), "utf8")).toBe("hello\r\nadded\r\nworld\r\n");
  });

  test("ignores a picked file that arrives after a newer daemon open", async ({ page }) => {
    await writeFile(join(root, "newer.md"), "newer\n");
    await stubPicker(page, "stale.md", "stale\n", { gatePick: true });
    await openDemo(page);

    // The pick is held, so this open is still in the air when the daemon one is asked for.
    await page.click("#local-open");
    await openThroughDaemon(page, daemon, "newer.md");
    await expect(statusOf(page)).toHaveText("newer.md を開きました");

    await page.evaluate(() => window.wimReleasePick());

    // The older ask finishing last leaves nothing behind: the buffer, what is reported and the
    // connection `:w` writes over all stay the ones the newer open put there.
    expect(await page.evaluate(() => window.wimDemo.state().lines[0])).toBe("newer");
    await editFirstLine(page);
    await write(page);

    await expect(statusOf(page)).toHaveText("newer.md を保存しました");
    expect(await readFile(join(root, "newer.md"), "utf8")).toBe("wim newer\n");
    expect(await page.evaluate(() => window.wimFile.written)).toBeNull();
  });

  test("finishes the saves queued for a file before closing its connection", async ({ page }) => {
    await writeFile(join(root, "queued.md"), "hello\n");
    await writeFile(join(root, "next.md"), "next\n");
    await gateDaemonSocket(page, { holdWrites: true });
    await openDemo(page);

    await openThroughDaemon(page, daemon, "queued.md");
    await expect(statusOf(page)).toHaveText("queued.md を開きました");

    await insert(page, "A");
    await write(page);
    await insert(page, "B");
    await write(page);

    // The first save is held on its way out and the second one is queued behind it, so neither
    // has reached the file when the next one is opened.
    expect(await readFile(join(root, "queued.md"), "utf8")).toBe("hello\n");

    await openThroughDaemon(page, daemon, "next.md");
    await expect(statusOf(page)).toHaveText("next.md を開きました");

    await page.evaluate(() => window.wimReleaseDaemonWrites());

    // Both saves land, in the order they were typed, over the connection they were typed for:
    // the file that was open when they were queued holds the text of the second `:w`.
    await expect.poll(() => readFile(join(root, "queued.md"), "utf8")).toBe("BAhello\n");
    await expect(statusOf(page)).toHaveText("queued.md を保存しました");
  });

  test("keeps an open that is still connecting when a picker is canceled", async ({ page }) => {
    await writeFile(join(root, "connecting.md"), "connecting\n");
    await gateDaemonSocket(page, { holdOpen: true });
    await stubCanceledPicker(page);
    await openDemo(page);

    await openThroughDaemon(page, daemon, "connecting.md");
    await expect(statusOf(page)).toHaveText("デーモンに接続しています");

    // Opening the picker and closing it again asks for no file at all, so the open still in the
    // air is still the one being waited for.
    await page.click("#local-open");
    expect(await page.evaluate(() => window.wimPickerCalls)).toBe(1);

    await page.evaluate(() => window.wimReleaseConnect());

    await expect(statusOf(page)).toHaveText("connecting.md を開きました");
    expect(await page.evaluate(() => window.wimDemo.state().lines[0])).toBe("connecting");
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

  test("refuses a picked file that is not UTF-8", async ({ page }) => {
    // 「あ」 in Shift-JIS, which is not a UTF-8 sequence: 0x82 starts none and 0xa0 continues none.
    await stubPicker(page, "sjis.txt", [0x82, 0xa0, 0x0a]);
    await openDemo(page);
    const before = await page.evaluate(() => window.wimDemo.state().text);

    await page.click("#local-open");

    await expect(statusOf(page)).toHaveText("UTF-8 ではないため開けません");
    expect(await page.evaluate(() => window.wimDemo.state().text)).toBe(before);

    // Nothing was opened, so a `:w` has no file to put a lossily decoded buffer back on.
    await page.click("h1");
    await write(page);
    await expect(statusOf(page)).toHaveText("開いているファイルがありません");
    expect(await page.evaluate(() => window.wimFile.written)).toBeNull();
  });

  test("saves a CRLF file back with the line endings it was opened with", async ({ page }) => {
    await stubPicker(page, "crlf.md", "hello\r\nworld\r\n");
    await openDemo(page);

    await page.click("#local-open");

    await expect(statusOf(page)).toHaveText("crlf.md を開きました");
    // A trailing newline terminates the last line rather than opening an empty one, so the
    // buffer holds the two lines the file has.
    expect(await page.evaluate(() => window.wimDemo.state().lines)).toEqual(["hello", "world"]);

    await openLineBelow(page, "added");
    await write(page);

    await expect(statusOf(page)).toHaveText("crlf.md を保存しました");
    expect(await page.evaluate(() => window.wimFile.written)).toBe(
      "hello\r\nadded\r\nworld\r\n",
    );
  });

  test("puts two saves on the file in the order they were typed", async ({ page }) => {
    await stubPicker(page, "local.md", "hello\n", { gateFirstWrite: true });
    await openDemo(page);
    await page.click("#local-open");
    await expect(statusOf(page)).toHaveText("local.md を開きました");

    await insert(page, "A");
    await write(page);
    await insert(page, "B");
    await write(page);

    // The first save is held open, and the second one waits for it rather than going around it.
    expect(await page.evaluate(() => window.wimFile.writes)).toEqual([]);

    await page.evaluate(() => window.wimReleaseWrite());

    await expect
      .poll(() => page.evaluate(() => window.wimFile.writes))
      .toEqual(["Ahello\n", "BAhello\n"]);
    await expect(statusOf(page)).toHaveText("local.md を保存しました");
  });

  test("is offered only by a browser that has the API", async ({ page }) => {
    await page.addInitScript(() => {
      delete window.showOpenFilePicker;
    });
    await openDemo(page);

    await expect(page.locator("#local-open")).toBeDisabled();
  });
});
