import { expect, test } from "@playwright/test";

/** The line the demo starts with, which the tests edit. */
const FIRST_LINE = "wim is a Vim-grammar editor, not a Vim clone.";

/** The colours `main.js` draws in, which is what lets a test tell them apart in the pixels. */
const BACKGROUND = [18, 20, 26];
const CURSOR = [92, 156, 245];

/**
 * The effects that ask something of the host, which is everything but the events the core
 * reports about itself. What those are and when they are raised is checked on its own
 * (`e2e/autocmd.spec.js`).
 */
function requests(state) {
  return state.effects.filter((effect) => effect.kind !== "event");
}

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

/**
 * Drives an IME the way the platform's own does, through Chrome's input protocol.
 *
 * `Input.imeSetComposition` is the composition a real IME puts up — it raises
 * `compositionstart` and `compositionupdate` on whatever is focused, and nothing at all when
 * that is not something text can be composed into — and `Input.insertText` is the confirmation,
 * which ends the composition and hands over the text. Synthesising `CompositionEvent`s by hand
 * would test the demo's handlers against events it wrote itself; this way the browser decides
 * what the demo hears, the same as with a keyboard.
 */
async function ime(page) {
  const session = await page.context().newCDPSession(page);
  return {
    /** Puts `text` up as the unconfirmed composition, with the caret at its end. */
    compose: (text) =>
      session.send("Input.imeSetComposition", {
        text,
        selectionStart: text.length,
        selectionEnd: text.length,
      }),
    /** Drops the composition, which is what an IME does when it is cancelled. */
    cancel: () =>
      session.send("Input.imeSetComposition", { text: "", selectionStart: 0, selectionEnd: 0 }),
    /** Confirms the composition as `text`. */
    commit: (text) => session.send("Input.insertText", { text }),
  };
}

/**
 * What the demo is composing and where it draws it, as `{ text, visible, left, top }`.
 *
 * The composition is drawn into the row the cursor is on rather than into an element of its own,
 * so where it starts is the cursor the demo reports. That it reaches the pixels there is what
 * "a composition pushes the rest of the line right instead of hiding it" checks.
 */
async function preeditOf(page) {
  const state = await page.evaluate(() => window.wimDemo.state());
  return {
    text: state.ime.composition,
    visible: state.ime.composition !== "",
    left: state.ime.cursor.x,
    top: state.ime.cursor.y,
  };
}

test.beforeEach(async ({ page }) => {
  await page.goto("/index.html");
  await page.waitForFunction(() => window.wimDemo !== undefined);
  // The editor holds the keys it is pressed with until the autocmds are in, so that no event goes
  // out over a config that has not been read yet (`web/main.js`). Waiting for that is what makes
  // a key pressed here one the editor types rather than one it queues.
  await page.evaluate(() => window.wimDemo.autocmds());
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
  // Firefox on Windows raises the AltGraph state for plain Ctrl+Alt, but the plain letter
  // says it is a shortcut all the same.
  expect(
    await dispatchKey(page, { key: "f", altKey: true, ctrlKey: true, modifierAltGraph: true }),
  ).toBe(false);
  // Alt with a plain letter is the browser's menu shortcut, not typing.
  expect(await dispatchKey(page, { key: "f", altKey: true })).toBe(false);
  // Alt+Space is the window's system menu; the space AltGr types is U+00A0, not this one.
  expect(await dispatchKey(page, { key: " ", altKey: true })).toBe(false);
  // A key inside a dead-key or IME composition is the browser's until the character is done.
  expect(await dispatchKey(page, { key: "e", isComposing: true })).toBe(false);

  expect(await page.evaluate(() => window.wimDemo.state().lines[0])).toBe(`@€${FIRST_LINE}`);
});

test("a composition is drawn into the row until the IME confirms it", async ({ page }) => {
  await page.keyboard.press("i");
  expect(await page.evaluate(() => window.wimDemo.state().ime.focused)).toBe(true);

  const composer = await ime(page);
  await composer.compose("にほんご");

  const composing = await page.evaluate(() => window.wimDemo.state());
  expect(composing.ime.composition).toBe("にほんご");
  // Nothing is confirmed yet, so the core has not been told about any of it.
  expect(composing.lines[0]).toBe(FIRST_LINE);
  expect(composing.cursor).toEqual({ line: 0, col: 0 });

  const preedit = await preeditOf(page);
  expect(preedit).toMatchObject({ text: "にほんご", visible: true });
  // Drawn where the cursor is, which on the first column of the first line is the top left of
  // the text area.
  expect(preedit.left).toBeCloseTo(composing.layout.textLeft);
  expect(preedit.top).toBe(composing.layout.padding);

  await composer.commit("日本語");

  const committed = await page.evaluate(() => window.wimDemo.state());
  expect(committed.lines[0]).toBe(`日本語${FIRST_LINE}`);
  // Three graphemes typed, each of them two cells wide, so the cursor is three columns and six
  // cells along.
  expect(committed.cursor).toEqual({ line: 0, col: 3 });
  expect(committed.damage).toEqual({ start: 0, end: 1 });
  expect(committed.ime.cursor.x).toBeCloseTo(
    committed.layout.textLeft + 6 * committed.layout.cellWidth,
  );
  expect(committed.ime.composition).toBe("");
  expect(await preeditOf(page)).toMatchObject({ visible: false });
});

test("a composition pushes the rest of the line right instead of hiding it", async ({ page }) => {
  await page.evaluate(() => window.wimDemo.load("ABC"));
  await page.keyboard.press("i");
  const canvas = page.locator("#screen");

  // `にほん` is three full-width graphemes, so six cells: composing it at column zero moves `BC`
  // — the suffix past the character the caret sits on — from the second cell to the eighth.
  const suffixArea = (state) =>
    rowArea(
      state,
      0,
      state.layout.textLeft + 7 * state.layout.cellWidth,
      2 * state.layout.cellWidth,
    );

  const before = await page.evaluate(() => window.wimDemo.state());
  const empty = await pixelsIn(page, suffixArea(before));
  expect(empty.filter((pixel) => !isColour(pixel, BACKGROUND))).toHaveLength(0);
  const plain = await canvas.evaluate((element) => element.toDataURL());

  const composer = await ime(page);
  await composer.compose("にほん");

  const composing = await page.evaluate(() => window.wimDemo.state());
  // None of it is confirmed, so the line the core holds is still the one it started with.
  expect(composing.lines[0]).toBe("ABC");
  const suffix = await pixelsIn(page, suffixArea(composing));
  expect(suffix.filter((pixel) => !isColour(pixel, BACKGROUND)).length).toBeGreaterThan(0);

  // What is being composed is underlined, in the colour the cursor is drawn in, over the six
  // cells it occupies.
  const preedit = await pixelsIn(
    page,
    rowArea(composing, 0, composing.layout.textLeft, 6 * composing.layout.cellWidth),
  );
  expect(preedit.filter((pixel) => isColour(pixel, CURSOR)).length).toBeGreaterThan(0);

  // A composition the IME drops takes the row back to the pixels it had before it opened.
  await composer.cancel();
  expect(await page.evaluate(() => window.wimDemo.state().ime.composition)).toBe("");
  expect(await canvas.evaluate((element) => element.toDataURL())).toBe(plain);
});

test("Normal mode has nothing for an IME to compose into", async ({ page }) => {
  const composer = await ime(page);
  expect(await page.evaluate(() => window.wimDemo.state().ime.focused)).toBe(false);

  await composer.compose("にほん");
  await composer.commit("日本");

  const state = await page.evaluate(() => window.wimDemo.state());
  expect(state.lines[0]).toBe(FIRST_LINE);
  expect(state.mode).toBe("NORMAL");
  expect(state.ime.composition).toBe("");
  expect(await preeditOf(page)).toMatchObject({ visible: false });

  // Keys are still read as commands rather than as text to compose.
  await page.keyboard.press("x");
  expect(await page.evaluate(() => window.wimDemo.state().lines[0])).toBe(FIRST_LINE.slice(1));
});

test("Esc belongs to the IME while it is composing", async ({ page }) => {
  await page.keyboard.press("i");

  // Every key of a composition is the IME's, the named ones included: Enter confirms, Esc
  // abandons and Backspace edits what is being composed.
  for (const key of ["Escape", "Enter", "Backspace", "Tab"]) {
    expect(await dispatchKey(page, { key, isComposing: true })).toBe(false);
  }

  const composer = await ime(page);
  await composer.compose("にほん");
  await page.keyboard.press("Escape");

  const composing = await page.evaluate(() => window.wimDemo.state());
  // The Esc went to the IME, so the editor is still where it was, with the composition up.
  expect(composing.mode).toBe("INSERT");
  expect(composing.ime.composition).toBe("にほん");
  expect(composing.lines[0]).toBe(FIRST_LINE);

  // A real IME answers that Esc by dropping the composition, which is what the protocol's empty
  // composition stands for here.
  await composer.cancel();
  const dropped = await page.evaluate(() => window.wimDemo.state());
  expect(dropped.mode).toBe("INSERT");
  expect(dropped.ime.composition).toBe("");
  expect(dropped.lines[0]).toBe(FIRST_LINE);
  expect(await preeditOf(page)).toMatchObject({ visible: false });

  // With the composition gone the next Esc is the editor's again, and the mode it leaves behind
  // takes keys as commands.
  await page.keyboard.press("Escape");
  const normal = await page.evaluate(() => window.wimDemo.state());
  expect(normal.mode).toBe("NORMAL");
  expect(normal.ime.focused).toBe(false);

  await page.keyboard.press("x");
  expect(await page.evaluate(() => window.wimDemo.state().lines[0])).toBe(FIRST_LINE.slice(1));
});

test("the key code an IME is still processing stays with it", async ({ page }) => {
  await page.keyboard.press("i");
  const before = await page.evaluate(() => window.wimDemo.state().lines);

  // Safari confirms a composition with a keydown that says `isComposing` is false and carries
  // only the reserved key code 229 to say the IME is handling it. Taking that Enter would split
  // the line under the text the IME is about to hand over.
  for (const key of ["Enter", "Escape", "Backspace", "Process"]) {
    expect(await dispatchKey(page, { key, keyCode: 229 })).toBe(false);
  }

  const state = await page.evaluate(() => window.wimDemo.state());
  expect(state.mode).toBe("INSERT");
  expect(state.lines).toEqual(before);
  expect(state.cursor).toEqual({ line: 0, col: 0 });

  // A key that carries its own code is the editor's, so the guard costs nothing outside a
  // composition: this Esc leaves Insert mode.
  expect(await dispatchKey(page, { key: "Escape", keyCode: 27 })).toBe(true);
  expect(await page.evaluate(() => window.wimDemo.state().mode)).toBe("NORMAL");
});

test("the command line takes composed text too", async ({ page }) => {
  await page.keyboard.type(":w ");
  const opened = await page.evaluate(() => window.wimDemo.state());
  expect(opened.mode).toBe("COMMAND");
  expect(opened.ime.focused).toBe(true);

  const composer = await ime(page);
  await composer.compose("めも");

  const composing = await page.evaluate(() => window.wimDemo.state());
  expect(composing.ime.composition).toBe("めも");
  expect(composing.commandLine).toBe(":w ");
  // The command line is typed into the status line, so that is where the composition is drawn:
  // under every row of the buffer, three cells along for the `:w ` already typed.
  const preedit = await preeditOf(page);
  expect(preedit).toMatchObject({ text: "めも", visible: true });
  expect(preedit.left).toBeCloseTo(composing.layout.padding + 3 * composing.layout.cellWidth);
  expect(preedit.top).toBeGreaterThanOrEqual(
    composing.layout.padding + composing.viewport.rows * composing.layout.lineHeight,
  );

  await composer.commit("メモ.txt");
  expect(await page.evaluate(() => window.wimDemo.state().commandLine)).toBe(":w メモ.txt");

  await page.keyboard.press("Enter");
  const state = await page.evaluate(() => window.wimDemo.state());
  expect(state.mode).toBe("NORMAL");
  expect(requests(state)).toEqual([{ kind: "save", path: "メモ.txt" }]);
  expect(state.ime.focused).toBe(false);
});

test("composed text that reads as key notation is typed rather than run", async ({ page }) => {
  await page.keyboard.press("i");

  const composer = await ime(page);
  await composer.compose("<");
  await composer.commit("<Esc>x");

  const state = await page.evaluate(() => window.wimDemo.state());
  expect(state.lines[0]).toBe(`<Esc>x${FIRST_LINE}`);
  // The `<Esc>` went in as the six characters it is, so the editor never left Insert mode.
  expect(state.mode).toBe("INSERT");
});

test("an Ex command hands its effect back to the host", async ({ page }) => {
  await page.keyboard.type(":w notes.txt");
  expect(await page.evaluate(() => window.wimDemo.state().commandLine)).toBe(":w notes.txt");

  await page.keyboard.press("Enter");

  const state = await page.evaluate(() => window.wimDemo.state());
  expect(state.mode).toBe("NORMAL");
  expect(requests(state)).toEqual([{ kind: "save", path: "notes.txt" }]);
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
