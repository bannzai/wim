import { expect, test } from "@playwright/test";

/** The line the demo starts with, which the tests edit. */
const FIRST_LINE = "wim is a Vim-grammar editor, not a Vim clone.";

/** The colours `main.js` draws in, which is what lets a test tell them apart in the pixels. */
const BACKGROUND = [18, 20, 26];
const CURSOR = [92, 156, 245];

/** A buffer of `count` numbered lines, taller than the viewport when `count` is large. */
function numberedLines(count) {
  return Array.from({ length: count }, (_, line) => `line ${line + 1}`).join("\n");
}

/**
 * The pixels of a CSS-pixel rectangle of the canvas, as `{ x, y, red, green, blue }` rows in
 * CSS pixels, so that a test can compare where something was drawn against the layout the demo
 * reports.
 */
function pixelsIn(page, rectangle) {
  return page.evaluate((area) => {
    const canvas = document.querySelector("#screen");
    const scale = window.devicePixelRatio || 1;
    const left = Math.round(area.x * scale);
    const top = Math.round(area.y * scale);
    const width = Math.max(1, Math.round(area.width * scale));
    const height = Math.max(1, Math.round(area.height * scale));
    const { data } = canvas.getContext("2d").getImageData(left, top, width, height);
    const pixels = [];
    for (let pixel = 0; pixel < data.length; pixel += 4) {
      const index = pixel / 4;
      pixels.push({
        x: (left + (index % width)) / scale,
        y: (top + Math.floor(index / width)) / scale,
        red: data[pixel],
        green: data[pixel + 1],
        blue: data[pixel + 2],
      });
    }
    return pixels;
  }, rectangle);
}

/** The rectangle of the row `line` sits on, in CSS pixels. */
function rowArea(state, line, x, width) {
  return {
    x,
    y: state.layout.padding + (line - state.viewport.top) * state.layout.lineHeight,
    width,
    height: state.layout.lineHeight,
  };
}

function isColour(pixel, [red, green, blue]) {
  return pixel.red === red && pixel.green === green && pixel.blue === blue;
}

/**
 * Dispatches a synthetic keydown carrying `init`, answering whether the demo took the key.
 *
 * `page.keyboard` cannot hold AltGr down, and a key the demo leaves alone is only observable as
 * the `preventDefault` it did not call, which a dispatched event reports back.
 */
function dispatchKey(page, init) {
  return page.evaluate(
    (options) =>
      !window.dispatchEvent(new KeyboardEvent("keydown", { ...options, cancelable: true })),
    init,
  );
}

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
  // Past the end of the buffer left behind: the row the last line vacated has to be drawn over.
  expect(state.damage).toEqual({ start: 0, end: before.length });
});

test("deleting the last line damages the row it was drawn on", async ({ page }) => {
  await page.evaluate((text) => window.wimDemo.load(text), numberedLines(5));
  const canvas = page.locator("#screen");

  await page.keyboard.press("Shift+G");
  await page.keyboard.press("d");
  await page.keyboard.press("d");

  const state = await page.evaluate(() => window.wimDemo.state());
  expect(state.lines).toEqual(["line 1", "line 2", "line 3", "line 4"]);
  expect(state.damage).toEqual({ start: 4, end: 5 });

  // The row now past the end of the buffer carries the filler a full redraw draws there, rather
  // than the text it held before the deletion.
  const damaged = await canvas.evaluate((element) => element.toDataURL());
  expect(
    await canvas.evaluate((element) => {
      window.wimDemo.redraw();
      return element.toDataURL();
    }),
  ).toBe(damaged);
});

test("Ctrl combinations the core has no command for stay with the browser", async ({ page }) => {
  // Redo is the one Ctrl key the grammar reads, so the demo takes it.
  expect(await dispatchKey(page, { key: "r", ctrlKey: true })).toBe(true);
  for (const key of ["f", "p", "s"]) {
    expect(await dispatchKey(page, { key, ctrlKey: true })).toBe(false);
  }

  // None of them reached the editor, so the buffer is the one the page loaded with.
  const state = await page.evaluate(() => window.wimDemo.state());
  expect(state.lines[0]).toBe(FIRST_LINE);
  expect(state.mode).toBe("NORMAL");
});

test("AltGr and Option type their character rather than a shortcut", async ({ page }) => {
  await page.keyboard.press("i");

  // A German layout reports AltGr+Q as `@`, with both Alt and Ctrl held.
  expect(
    await dispatchKey(page, { key: "@", altKey: true, ctrlKey: true, modifierAltGraph: true }),
  ).toBe(true);
  // macOS reports Option+2 as `€`, with Alt alone.
  expect(await dispatchKey(page, { key: "€", altKey: true })).toBe(true);
  // Ctrl and Alt without AltGr remains a shortcut.
  expect(await dispatchKey(page, { key: "f", altKey: true, ctrlKey: true })).toBe(false);

  expect(await page.evaluate(() => window.wimDemo.state().lines[0])).toBe(`@€${FIRST_LINE}`);
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

test("a buffer taller than the viewport scrolls to follow the cursor", async ({ page }) => {
  await page.evaluate((text) => window.wimDemo.load(text), numberedLines(200));

  const start = await page.evaluate(() => window.wimDemo.state());
  expect(start.viewport.top).toBe(0);
  expect(start.viewport.rows).toBeGreaterThan(0);
  expect(start.viewport.rows).toBeLessThan(200);

  await page.keyboard.press("Shift+G");
  const bottom = await page.evaluate(() => window.wimDemo.state());
  expect(bottom.cursor.line).toBe(199);
  // The last line sits on the last row of the viewport, which is where it lands as the cursor
  // walks off the bottom of the previous one.
  expect(bottom.viewport.top).toBe(200 - bottom.viewport.rows);

  await page.keyboard.press("g");
  await page.keyboard.press("g");
  const top = await page.evaluate(() => window.wimDemo.state());
  expect(top.cursor.line).toBe(0);
  expect(top.viewport.top).toBe(0);
});

test("the gutter carries the number of the line scrolled to the top", async ({ page }) => {
  await page.evaluate((text) => window.wimDemo.load(text), numberedLines(200));
  await page.keyboard.press("Shift+G");

  const state = await page.evaluate(() => window.wimDemo.state());
  const gutter = await pixelsIn(page, rowArea(state, state.viewport.top, 0, state.layout.textLeft));
  expect(gutter.filter((pixel) => !isColour(pixel, BACKGROUND)).length).toBeGreaterThan(0);
});

test("a full-width grapheme is drawn two cells wide", async ({ page }) => {
  await page.evaluate(() => window.wimDemo.load("あい 漢字 😀\nplain ascii"));

  // `l` steps one column, which is one grapheme, so the cursor lands past two cells of `あ`.
  await page.keyboard.press("l");
  const state = await page.evaluate(() => window.wimDemo.state());
  expect(state.cursor).toEqual({ line: 0, col: 1 });

  const width = state.layout.textLeft + 20 * state.layout.cellWidth;
  const row = await pixelsIn(page, rowArea(state, 0, 0, width));
  const cursor = row.filter((pixel) => isColour(pixel, CURSOR));
  expect(cursor.length).toBeGreaterThan(0);
  const left = Math.min(...cursor.map((pixel) => pixel.x));
  const right = Math.max(...cursor.map((pixel) => pixel.x));
  // Within a pixel, because the cell width is fractional and the edge of the block is blended.
  expect(Math.abs(left - (state.layout.textLeft + 2 * state.layout.cellWidth))).toBeLessThan(1.5);
  expect(Math.abs(right - left - 2 * state.layout.cellWidth)).toBeLessThan(1.5);

  // The glyphs themselves reach the canvas rather than the row being cursor and background.
  const text = row.filter(
    (pixel) =>
      pixel.x > state.layout.textLeft &&
      !isColour(pixel, BACKGROUND) &&
      !isColour(pixel, CURSOR),
  );
  expect(text.length).toBeGreaterThan(0);
});

test("redrawing only the damaged rows leaves the pixels a full redraw would", async ({ page }) => {
  await page.evaluate((text) => window.wimDemo.load(text), numberedLines(80));
  const canvas = page.locator("#screen");

  // An insert damages one line, `o` and `dd` damage every line under the cursor, and `G` moves
  // the viewport out from under all of them.
  await page.keyboard.press("j");
  await page.keyboard.press("i");
  await page.keyboard.type("あ edited ");
  await page.keyboard.press("Escape");
  await page.keyboard.press("o");
  await page.keyboard.type("opened");
  await page.keyboard.press("Escape");
  await page.keyboard.press("d");
  await page.keyboard.press("d");

  const damaged = await canvas.evaluate((element) => element.toDataURL());
  const complete = await canvas.evaluate((element) => {
    window.wimDemo.redraw();
    return element.toDataURL();
  });
  expect(damaged).toBe(complete);

  await page.keyboard.press("Shift+G");
  await page.keyboard.press("x");
  const scrolled = await canvas.evaluate((element) => element.toDataURL());
  expect(
    await canvas.evaluate((element) => {
      window.wimDemo.redraw();
      return element.toDataURL();
    }),
  ).toBe(scrolled);
});
