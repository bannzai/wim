// The demo host: it turns browser key events into the key notation wim-core reads, and draws
// the state the editor hands back onto a canvas.
//
// Drawing follows documents/PROJECT.md. Glyphs are baked once into an offscreen atlas and
// blitted from there rather than laid out again on every frame, and a key redraws only the
// lines the editor reports as damaged plus the rows the cursor left and landed on. Anything
// that moves every row — a scroll, a resize, a new buffer — redraws the lot.

import init, { WimEditor, display_cells } from "./pkg/wim_wasm.js";

const INITIAL_TEXT = `wim is a Vim-grammar editor, not a Vim clone.
The core is one pure Rust crate, compiled to Wasm for this page.

Type i to insert, Esc to go back, dd to delete this line.
全角も 1 桁ではなく 2 桁ぶんの幅で描く。`;

/** Font size in CSS pixels. The cell size is measured from it, so the two never drift. */
const FONT_SIZE = 16;
const LINE_HEIGHT = 22;
const PADDING = 12;
/** Height of the canvas in CSS pixels, which is what the `height` attribute in the page says. */
const HEIGHT = 440;

const COLORS = {
  background: "#12141a",
  text: "#d8dee9",
  muted: "#6a7383",
  cursor: "#5c9cf5",
};

/**
 * Line numbers narrower than this still get a gutter this wide, so that the text does not
 * shift sideways every time a buffer crosses ten or a hundred lines.
 */
const NUMBER_DIGITS = 3;

/**
 * Side of the glyph atlas in device pixels. 1024 holds around a thousand cells of this font
 * size, which is more than the glyphs one page of text uses; a buffer that outgrows it rebakes
 * from the top left.
 */
const ATLAS_SIZE = 1024;

/** The monospace stack at `size` pixels, which the canvas and the atlas share. */
function fontAt(size) {
  return `${size}px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace`;
}

const canvas = document.querySelector("#screen");
const context = canvas.getContext("2d");

/** The editor, which cannot be built before the wasm module is initialised. */
let editor;

/** What the last batch of keys did, which the demo shows in its status line. */
let lastOutcome = { damageStart: 0, damageEnd: 0, effects: [] };

/**
 * What the last frame was drawn with. A frame compares itself against this to work out which
 * rows it can leave alone, and the E2E run reads the viewport out of it.
 */
let view = {
  scale: 0,
  width: 0,
  cellWidth: 0,
  gutterWidth: 0,
  textLeft: 0,
  visibleRows: 0,
  scrollTop: 0,
  lineCount: 0,
  cursorLine: 0,
};

/** Glyphs baked at the current scale and cell size, keyed by the colour they were baked in. */
const atlas = {
  canvas: document.createElement("canvas"),
  context: null,
  /** `${color} ${grapheme}` to the device-pixel rectangle holding it. */
  slots: new Map(),
  x: 0,
  y: 0,
  rowHeight: 0,
  scale: 0,
  cellWidth: 0,
};

/**
 * The key notation `parse_keys` reads for `event`, or `null` for a key the core has no name
 * for — a bare modifier, an arrow, a browser shortcut — which the browser keeps.
 */
function keyNotation(event) {
  switch (event.key) {
    case "Escape":
      return "<Esc>";
    case "Enter":
      return "<CR>";
    case "Backspace":
      return "<BS>";
    case "Tab":
      return "<Tab>";
    default:
      break;
  }
  // Anything longer than one character is a named key the core does not know.
  if (event.key.length !== 1 || event.metaKey || event.altKey) {
    return null;
  }
  if (event.ctrlKey) {
    // The Ctrl keys the core reads are letters, and `<C-x>` is the only shape `parse_keys`
    // accepts, so a Ctrl combination on anything else is left to the browser.
    return /^[a-z]$/i.test(event.key) ? `<C-${event.key}>` : null;
  }
  return event.key === "<" ? "<lt>" : event.key;
}

function handleKeys(keys) {
  const outcome = editor.handle_keys(keys);
  lastOutcome = {
    damageStart: outcome.damage_start,
    damageEnd: outcome.damage_end,
    effects: JSON.parse(outcome.effects),
  };
  draw();
  return lastOutcome;
}

/** The cells `text` draws as: one per column the core counts, carrying its display width. */
function cellsOf(text) {
  return JSON.parse(display_cells(text));
}

function statusText() {
  const commandLine = editor.command_line();
  if (commandLine !== undefined) {
    return commandLine;
  }
  const position = `${editor.cursor_line() + 1},${editor.cursor_col() + 1}`;
  const error = lastOutcome.effects.find((effect) => effect.kind === "error");
  return error === undefined
    ? `-- ${editor.mode()} --  ${position}`
    : `-- ${editor.mode()} --  ${position}  ${error.message}`;
}

/** Rebakes the atlas at `scale` with cells `cellWidth` CSS pixels wide, dropping every glyph. */
function resetAtlas(scale, cellWidth) {
  atlas.canvas.width = ATLAS_SIZE;
  atlas.canvas.height = ATLAS_SIZE;
  atlas.context = atlas.canvas.getContext("2d");
  // The atlas is written in device pixels rather than under a transform, which is what lets a
  // slot start on a whole pixel and blit onto the screen one for one.
  atlas.context.font = fontAt(FONT_SIZE * scale);
  atlas.context.textBaseline = "top";
  atlas.slots.clear();
  atlas.x = 0;
  atlas.y = 0;
  atlas.rowHeight = Math.ceil(LINE_HEIGHT * scale);
  atlas.scale = scale;
  atlas.cellWidth = cellWidth;
}

/**
 * The atlas rectangle holding `text` in `color`, baking it on first use.
 *
 * The atlas is a cache with the one eviction a demo needs: a full atlas starts over from the
 * top left. Rows already on screen keep the pixels they were blitted, so a wrap costs a rebake
 * of the glyphs still in use and nothing else.
 */
function bakedGlyph(text, width, color) {
  const key = `${color} ${text}`;
  const cached = atlas.slots.get(key);
  if (cached !== undefined) {
    return cached;
  }

  const slotWidth = Math.ceil(atlas.cellWidth * width * atlas.scale);
  if (atlas.x + slotWidth > ATLAS_SIZE) {
    atlas.x = 0;
    atlas.y += atlas.rowHeight;
  }
  if (atlas.y + atlas.rowHeight > ATLAS_SIZE) {
    atlas.slots.clear();
    atlas.x = 0;
    atlas.y = 0;
  }
  const slot = { x: atlas.x, y: atlas.y, width: slotWidth, height: atlas.rowHeight };
  atlas.x += slotWidth;

  atlas.context.save();
  atlas.context.beginPath();
  atlas.context.rect(slot.x, slot.y, slot.width, slot.height);
  // A glyph the font draws wider than the cell it was measured for would otherwise bleed into
  // the slot next to it, which is a different character.
  atlas.context.clip();
  atlas.context.clearRect(slot.x, slot.y, slot.width, slot.height);
  atlas.context.fillStyle = color;
  atlas.context.fillText(text, slot.x, slot.y);
  atlas.context.restore();

  atlas.slots.set(key, slot);
  return slot;
}

/** Blits `text` at (`x`, `y`) in CSS pixels, snapped to a device pixel to keep the blit 1:1. */
function drawGlyph(text, width, color, x, y) {
  if (text === " ") {
    return;
  }
  const slot = bakedGlyph(text, width, color);
  context.drawImage(
    atlas.canvas,
    slot.x,
    slot.y,
    slot.width,
    slot.height,
    Math.round(x * atlas.scale) / atlas.scale,
    Math.round(y * atlas.scale) / atlas.scale,
    slot.width / atlas.scale,
    slot.height / atlas.scale,
  );
}

/** Draws `cells` from `x` rightwards, each over as many cells as its width says. */
function drawCells(cells, x, y, color) {
  let left = x;
  for (const cell of cells) {
    drawGlyph(cell.text, cell.width, color, left, y);
    left += cell.width * view.cellWidth;
  }
}

/** Draws the cursor under the text, so that the character it sits on stays readable. */
function drawCursor(cells, top) {
  const col = editor.cursor_col();
  let left = view.textLeft;
  for (const cell of cells.slice(0, col)) {
    left += cell.width * view.cellWidth;
  }
  // A cell past the end of the line is the one an empty line's cursor sits in, one wide.
  const width =
    editor.mode() === "INSERT" ? 2 : (cells[col]?.width ?? 1) * view.cellWidth;
  context.fillStyle = COLORS.cursor;
  context.fillRect(left, top, width, LINE_HEIGHT);
}

/** Redraws one line of the buffer over the background, leaving nothing of the old row. */
function drawRow(line) {
  const top = PADDING + (line - view.scrollTop) * LINE_HEIGHT;
  context.fillStyle = COLORS.background;
  context.fillRect(0, top, view.width, LINE_HEIGHT);
  if (line >= view.lineCount) {
    // Vim's filler for the rows under the end of the buffer.
    drawGlyph("~", 1, COLORS.muted, PADDING, top);
    return;
  }

  const number = String(line + 1);
  drawCells(
    cellsOf(number),
    view.textLeft - (number.length + 1) * view.cellWidth,
    top,
    COLORS.muted,
  );

  const cells = cellsOf(editor.line(line));
  if (line === view.cursorLine) {
    drawCursor(cells, top);
  }
  drawCells(cells, view.textLeft, top, COLORS.text);
}

function drawStatusLine(top) {
  context.fillStyle = COLORS.background;
  context.fillRect(0, top, view.width, LINE_HEIGHT);
  drawCells(cellsOf(statusText()), PADDING, top, COLORS.muted);
}

/** Where the viewport starts after following the cursor, which is what keeps it on screen. */
function scrolledTop(visibleRows) {
  // A buffer shorter than the viewport, or one that shrank under it, pins the top back up.
  const top = Math.min(view.scrollTop, Math.max(0, editor.line_count() - visibleRows));
  const cursorLine = editor.cursor_line();
  if (cursorLine < top) {
    return cursorLine;
  }
  if (cursorLine >= top + visibleRows) {
    return cursorLine - visibleRows + 1;
  }
  return top;
}

/**
 * The lines the last batch of keys left needing a redraw: the ones whose text changed, the
 * rows a deletion left past the end of the buffer, and the rows the cursor left and landed on.
 */
function damagedRows() {
  const rows = new Set([view.cursorLine, editor.cursor_line()]);
  if (lastOutcome.damageEnd > lastOutcome.damageStart) {
    // Lines that went away leave rows that used to hold text and now hold the end-of-buffer
    // filler, which the damage range itself stops short of.
    const end = Math.max(lastOutcome.damageEnd, view.lineCount);
    for (let line = lastOutcome.damageStart; line < end; line += 1) {
      rows.add(line);
    }
  }
  return rows;
}

function draw({ full = false } = {}) {
  const scale = window.devicePixelRatio || 1;
  const width = canvas.clientWidth;
  const deviceWidth = Math.round(width * scale);
  const deviceHeight = Math.round(HEIGHT * scale);
  if (canvas.width !== deviceWidth || canvas.height !== deviceHeight) {
    // The backing store is in device pixels and the drawing in CSS pixels, so the text stays
    // sharp on a display that has more of the former than the latter. Sizing it also wipes it.
    canvas.width = deviceWidth;
    canvas.height = deviceHeight;
    // Growing the backing store grows the element unless the CSS height pins it down.
    canvas.style.height = `${HEIGHT}px`;
    full = true;
  }
  // Sizing the canvas resets the context, so every frame sets the state it draws under.
  context.setTransform(scale, 0, 0, scale, 0, 0);
  context.font = fontAt(FONT_SIZE);
  context.textBaseline = "top";

  // One cell is the advance of a character in the monospace font, so the cursor lands on the
  // character the core says it is on whatever font the browser picked.
  const cellWidth = context.measureText("M").width;
  if (scale !== atlas.scale || cellWidth !== atlas.cellWidth) {
    // Glyphs are baked at one size, so a display or a font the page picked up late makes every
    // one of them stale.
    resetAtlas(scale, cellWidth);
    full = true;
  }

  const lineCount = editor.line_count();
  const digits = Math.max(NUMBER_DIGITS, String(lineCount).length);
  // The column of space between the numbers and the text is the one `set number` leaves.
  const gutterWidth = (digits + 1) * cellWidth;
  const statusTop = HEIGHT - PADDING - LINE_HEIGHT;
  const visibleRows = Math.max(1, Math.floor((statusTop - PADDING) / LINE_HEIGHT));
  const scrollTop = scrolledTop(visibleRows);
  if (
    width !== view.width ||
    gutterWidth !== view.gutterWidth ||
    visibleRows !== view.visibleRows ||
    scrollTop !== view.scrollTop
  ) {
    // Every row moves or changes shape, so nothing on screen can be kept.
    full = true;
  }

  const rows = full
    ? Array.from({ length: visibleRows }, (_, row) => scrollTop + row)
    : damagedRows();
  view = {
    scale,
    width,
    cellWidth,
    gutterWidth,
    textLeft: PADDING + gutterWidth,
    visibleRows,
    scrollTop,
    lineCount,
    cursorLine: editor.cursor_line(),
  };

  if (full) {
    context.fillStyle = COLORS.background;
    context.fillRect(0, 0, width, HEIGHT);
  }
  for (const line of rows) {
    if (line >= scrollTop && line < scrollTop + visibleRows) {
      drawRow(line);
    }
  }
  // The mode, the position and the command line change on keys that damage no text at all.
  drawStatusLine(statusTop);
}

await init();
editor = new WimEditor(INITIAL_TEXT);
draw({ full: true });

// Listening only once the editor exists is what keeps a key typed during the wasm fetch from
// reaching a demo that has nothing to type into.
window.addEventListener("keydown", (event) => {
  const keys = keyNotation(event);
  if (keys === null) {
    return;
  }
  event.preventDefault();
  handleKeys(keys);
});

window.addEventListener("resize", () => draw());

// The handle the E2E run drives and inspects the demo through.
window.wimDemo = {
  sendKeys: handleKeys,
  /** Replaces the buffer, which is how the E2E run gets one taller than the viewport. */
  load: (text) => {
    editor = new WimEditor(text);
    lastOutcome = { damageStart: 0, damageEnd: 0, effects: [] };
    view = { ...view, scrollTop: 0 };
    draw({ full: true });
  },
  /** Redraws every row, which the E2E run compares the damage-driven redraw against. */
  redraw: () => draw({ full: true }),
  state: () => ({
    text: editor.text(),
    lines: Array.from({ length: editor.line_count() }, (_, line) => editor.line(line)),
    cursor: { line: editor.cursor_line(), col: editor.cursor_col() },
    mode: editor.mode(),
    commandLine: editor.command_line() ?? null,
    damage: { start: lastOutcome.damageStart, end: lastOutcome.damageEnd },
    effects: lastOutcome.effects,
    viewport: { top: view.scrollTop, rows: view.visibleRows },
    layout: {
      cellWidth: view.cellWidth,
      textLeft: view.textLeft,
      lineHeight: LINE_HEIGHT,
      padding: PADDING,
    },
  }),
};
