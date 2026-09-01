// Syntax highlighting, checked where it ends up: the pixels of the rows it colours.
//
// The demo bakes a glyph into the atlas under the colour it is drawn in, so a keyword reaching
// the canvas in the keyword colour is the whole path — grammar fetched, buffer parsed, captures
// turned into runs, runs turned into cell colours — landing. What the runs say is checked
// alongside it, so that a test which fails says which half went wrong.

import { expect, test } from "@playwright/test";

/** The colours `main.js` and `highlight.js` draw in, which is what tells them apart in the pixels. */
const BACKGROUND = [18, 20, 26];
const TEXT = [216, 222, 233];
const KEYWORD = [199, 146, 234];
const STRING = [195, 232, 141];
const COMMENT = [92, 99, 111];

/** A Rust buffer whose second line opens with a keyword, and whose first is a comment. */
const RUST = `// a comment over the whole line
use std::collections::HashMap;
`;

/**
 * A Rust buffer that a block comment opened on its second line takes over: the rows under it stop
 * being code, and the row above it loses the call it held.
 */
const RIPPLE = `fn a() {
    let x = 1;
    let y = 2;
}
`;

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
      pixels.push([data[pixel], data[pixel + 1], data[pixel + 2]]);
    }
    return pixels;
  }, rectangle);
}

/** The rectangle covering columns `from` up to `to` of row `line`, in CSS pixels. */
function cellsArea(state, line, from, to) {
  return {
    x: state.layout.textLeft + from * state.layout.cellWidth,
    y: state.layout.padding + (line - state.viewport.top) * state.layout.lineHeight,
    width: (to - from) * state.layout.cellWidth,
    height: state.layout.lineHeight,
  };
}

/** How far `pixel` is from the background, which is how much of a glyph covered it. */
function coverage(pixel) {
  return Math.hypot(...pixel.map((channel, index) => channel - BACKGROUND[index]));
}

/**
 * Which of `candidates` the text in `pixels` was drawn in.
 *
 * A glyph is drawn in one colour and antialiased against the background, so each of its pixels
 * sits somewhere on the line from the background to that colour, and no pixel of a 16px stroke is
 * certain to be the colour itself. The direction the pixel furthest from the background lies in
 * names the colour whatever coverage the antialiasing gave it.
 */
function drawnColour(pixels, candidates) {
  const covered = pixels.filter((pixel) => coverage(pixel) > 0);
  expect(covered.length, "nothing was drawn over the background here").toBeGreaterThan(0);
  const furthest = covered.reduce((best, pixel) => (coverage(pixel) > coverage(best) ? pixel : best));
  const angle = (colour) => {
    const drawn = furthest.map((channel, index) => channel - BACKGROUND[index]);
    const wanted = colour.map((channel, index) => channel - BACKGROUND[index]);
    const dot = drawn.reduce((sum, channel, index) => sum + channel * wanted[index], 0);
    return dot / (Math.hypot(...drawn) * Math.hypot(...wanted));
  };
  return candidates.reduce((best, colour) => (angle(colour) > angle(best) ? colour : best));
}

/** The colour columns `from` up to `to` of row `line` are drawn in, out of `candidates`. */
async function colourOfCells(page, line, from, to, candidates) {
  const state = await page.evaluate(() => window.wimDemo.state());
  return drawnColour(await pixelsIn(page, cellsArea(state, line, from, to)), candidates);
}

/** The canvas as it stands, and as a redraw of every row would leave it. */
async function damagedAndComplete(page) {
  const canvas = page.locator("#screen");
  const damaged = await canvas.evaluate((element) => element.toDataURL());
  const complete = await canvas.evaluate((element) => {
    window.wimDemo.redraw();
    return element.toDataURL();
  });
  return { damaged, complete };
}

test.beforeEach(async ({ page }) => {
  await page.goto("/index.html");
  await page.waitForFunction(() => window.wimDemo !== undefined);
  // The editor holds the keys it is pressed with until the autocmds are in, so that no event goes
  // out over a config that has not been read yet (`web/main.js`). Waiting for that is what makes
  // a key pressed here one the editor types rather than one it queues.
  await page.evaluate(() => window.wimDemo.autocmds());
});

test("the sample buttons load a buffer and highlight it", async ({ page }) => {
  await page.click("#sample-rust");
  await page.waitForFunction(() => window.wimDemo.state().highlight.language === "rust");
  expect(await page.evaluate(() => window.wimDemo.state().lines[1])).toContain("use ");

  await page.click("#sample-markdown");
  await page.waitForFunction(() => window.wimDemo.state().highlight.language === "markdown");
  // The heading, which is what the markdown grammar's first capture on the row is over.
  expect(await colourOfCells(page, 0, 2, 10, [TEXT, KEYWORD])).toEqual(KEYWORD);
});

test("a Rust buffer draws its keywords and comments in colours of their own", async ({ page }) => {
  await page.evaluate((text) => window.wimDemo.load(text, "buffer.rs"), RUST);
  expect(await page.evaluate(() => window.wimDemo.state().highlight.language)).toBe("rust");

  // `use` on the second row, which the query captures as a keyword.
  expect(await page.evaluate(() => window.wimDemo.highlightRuns(1))).toContainEqual({
    start: 0,
    end: 3,
    color: "#c792ea",
  });
  expect(await colourOfCells(page, 1, 0, 3, [TEXT, KEYWORD, COMMENT])).toEqual(KEYWORD);
  // The rest of that row is a path and a name, which nothing colours.
  expect(await colourOfCells(page, 1, 4, 20, [TEXT, KEYWORD, COMMENT])).toEqual(TEXT);
  // The first row is a comment. Its first column is under the cursor, so the reading starts past it.
  expect(await colourOfCells(page, 0, 3, 20, [TEXT, KEYWORD, COMMENT])).toEqual(COMMENT);
});

test("a Markdown buffer draws its headings and its fences", async ({ page }) => {
  await page.evaluate(
    (text) => window.wimDemo.load(text, "notes.md"),
    "# heading\n\nplain paragraph\n\n```rust\nfn a() {}\n```\n",
  );
  expect(await page.evaluate(() => window.wimDemo.state().highlight.language)).toBe("markdown");

  expect(await page.evaluate(() => window.wimDemo.highlightRuns(0))).toContainEqual({
    start: 2,
    end: 9,
    color: "#c792ea",
  });
  expect(await colourOfCells(page, 0, 2, 9, [TEXT, KEYWORD, STRING])).toEqual(KEYWORD);
  // A paragraph is text and stays the colour text is drawn in.
  expect(await page.evaluate(() => window.wimDemo.highlightRuns(2))).toEqual([]);
  expect(await colourOfCells(page, 2, 0, 15, [TEXT, KEYWORD, STRING])).toEqual(TEXT);
  // The language a fence opens with belongs to the fence, while the code under it is taken back to
  // plain by the grammar's own `@none` inside the capture covering the whole block.
  expect(await colourOfCells(page, 4, 3, 7, [TEXT, KEYWORD, STRING])).toEqual(STRING);
  expect(await page.evaluate(() => window.wimDemo.highlightRuns(5))).toEqual([]);
  expect(await colourOfCells(page, 5, 0, 9, [TEXT, KEYWORD, STRING])).toEqual(TEXT);
});

test("a buffer in no language the demo knows is drawn in the plain text colour", async ({
  page,
}) => {
  // The buffer the demo starts on has no name at all, and a file whose extension names no grammar
  // is the same thing: neither is parsed, and neither may lose a colour it used to be drawn in.
  expect(await page.evaluate(() => window.wimDemo.state().highlight.language)).toBeNull();
  expect(await page.evaluate(() => window.wimDemo.highlightRuns(0))).toBeNull();

  await page.evaluate(
    (text) => window.wimDemo.load(text, "notes.txt"),
    "fn main() {\n    let x = 1;\n}\n",
  );
  expect(await page.evaluate(() => window.wimDemo.state().highlight.language)).toBeNull();
  expect(await page.evaluate(() => window.wimDemo.highlightRuns(0))).toBeNull();
  expect(await colourOfCells(page, 0, 3, 9, [TEXT, KEYWORD, COMMENT])).toEqual(TEXT);

  // Editing it goes on redrawing the damage the editor reported and nothing else.
  await page.keyboard.press("j");
  await page.evaluate(() => window.wimDemo.sendKeys("iedited<Esc>"));
  const { damaged, complete } = await damagedAndComplete(page);
  expect(damaged).toBe(complete);
});

test("rows the edit never touched are redrawn when their highlighting changes", async ({
  page,
}) => {
  await page.evaluate((text) => window.wimDemo.load(text, "ripple.rs"), RIPPLE);
  // `a` on the first row is the name of a function, and `let` on the third row is a keyword.
  expect(await page.evaluate(() => window.wimDemo.highlightRuns(0))).toContainEqual({
    start: 3,
    end: 4,
    color: "#82aaff",
  });
  expect(await colourOfCells(page, 2, 4, 7, [TEXT, KEYWORD])).toEqual(KEYWORD);

  // A block comment opened on the second row swallows the rows under it and takes apart the
  // function on the row above it. wim-core damages the row the keys were typed on and no other,
  // because it has no idea that any of that happened.
  await page.evaluate(() => window.wimDemo.sendKeys("ji/*<Esc>"));
  const state = await page.evaluate(() => window.wimDemo.state());
  expect(state.damage.start).toBeGreaterThan(0);
  expect(state.highlight.damage).toEqual(expect.arrayContaining([0, 2]));
  expect(await page.evaluate(() => window.wimDemo.highlightRuns(0))).not.toContainEqual({
    start: 3,
    end: 4,
    color: "#82aaff",
  });
  expect(await page.evaluate(() => window.wimDemo.highlightRuns(2))).toEqual([]);
  expect(await colourOfCells(page, 2, 4, 7, [TEXT, KEYWORD])).toEqual(TEXT);

  // Which is only what is on screen because those rows were redrawn along with the damaged one.
  const { damaged, complete } = await damagedAndComplete(page);
  expect(damaged).toBe(complete);
});
