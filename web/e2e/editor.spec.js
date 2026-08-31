import { expect, test } from "@playwright/test";

/** The line the demo starts with, which the tests edit. */
const FIRST_LINE = "wim is a Vim-grammar editor, not a Vim clone.";

test.beforeEach(async ({ page }) => {
  await page.goto("/index.html");
  await page.waitForFunction(() => window.wimDemo !== undefined);
});

test("starts in Normal mode over the initial buffer", async ({ page }) => {
  const state = await page.evaluate(() => window.wimDemo.state());
  expect(state.mode).toBe("NORMAL");
  expect(state.lines[0]).toBe(FIRST_LINE);
  expect(state.cursor).toEqual({ line: 0, col: 0 });
});

test("typed keys insert text and Esc goes back to Normal mode", async ({ page }) => {
  await page.keyboard.press("i");
  expect(await page.evaluate(() => window.wimDemo.state().mode)).toBe("INSERT");

  await page.keyboard.type("hello ");
  const typed = await page.evaluate(() => window.wimDemo.state());
  expect(typed.lines[0]).toBe(`hello ${FIRST_LINE}`);
  expect(typed.damage).toEqual({ start: 0, end: 1 });

  await page.keyboard.press("Escape");

  const state = await page.evaluate(() => window.wimDemo.state());
  expect(state.mode).toBe("NORMAL");
  // `<Esc>` steps the cursor back off the character just typed, as Vim's does, and leaves the
  // text alone, so it damages no line.
  expect(state.cursor).toEqual({ line: 0, col: 5 });
  expect(state.damage).toEqual({ start: 0, end: 0 });
});

test("dd deletes a line and reports damage down to the end of the buffer", async ({ page }) => {
  const before = await page.evaluate(() => window.wimDemo.state().lines);

  await page.keyboard.press("d");
  await page.keyboard.press("d");

  const state = await page.evaluate(() => window.wimDemo.state());
  expect(state.lines).toEqual(before.slice(1));
  expect(state.damage).toEqual({ start: 0, end: before.length - 1 });
});

test("an Ex command hands its effect back to the host", async ({ page }) => {
  await page.keyboard.type(":w notes.txt");
  expect(await page.evaluate(() => window.wimDemo.state().commandLine)).toBe(":w notes.txt");

  await page.keyboard.press("Enter");

  const state = await page.evaluate(() => window.wimDemo.state());
  expect(state.mode).toBe("NORMAL");
  expect(state.effects).toEqual([{ kind: "save", path: "notes.txt" }]);
});

test("the canvas is drawn and redrawn as keys come in", async ({ page }) => {
  const canvas = page.locator("#screen");
  // A blank canvas is a uniform image; anything drawn leaves pixels off the background.
  const drawnPixels = await canvas.evaluate((element) => {
    const { data } = element
      .getContext("2d")
      .getImageData(0, 0, element.width, element.height);
    let drawn = 0;
    for (let pixel = 0; pixel < data.length; pixel += 4) {
      if (data[pixel] !== data[0] || data[pixel + 1] !== data[1] || data[pixel + 2] !== data[2]) {
        drawn += 1;
      }
    }
    return drawn;
  });
  expect(drawnPixels).toBeGreaterThan(0);

  const before = await canvas.evaluate((element) => element.toDataURL());
  await page.keyboard.press("i");
  await page.keyboard.type("hello");
  expect(await canvas.evaluate((element) => element.toDataURL())).not.toBe(before);
});
