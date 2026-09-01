// The demo host: it turns browser key events into the key notation wim-core reads, and draws
// the state the editor hands back onto a canvas.
//
// Drawing follows documents/PROJECT.md. Glyphs are baked once into an offscreen atlas and
// blitted from there rather than laid out again on every frame, and a key redraws only the
// lines the editor reports as damaged plus the rows the cursor left and landed on. Anything
// that moves every row — a scroll, a resize, a new buffer — redraws the lot.

import init, { WimEditor, display_cells } from "./pkg/wim_wasm.js";
import { connect } from "./daemon.js";

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

/** The textarea an IME composes into, which is focused only in the modes whose keys are text. */
const imeInput = document.querySelector("#ime");

/** The overlay the composition is drawn in, over the canvas at the cursor. */
const preedit = document.querySelector("#preedit");

/** The form that opens a file through a daemon, and the fields naming which one. */
const daemonForm = document.querySelector("#daemon-form");
const daemonAddress = document.querySelector("#daemon-address");
const daemonToken = document.querySelector("#daemon-token");
const daemonPath = document.querySelector("#daemon-path");

/** The button that opens a file the browser itself hands over and writes back. */
const localButton = document.querySelector("#local-open");

/** Where opening and saving report what they did, under the file controls. */
const fileStatus = document.querySelector("#file-status");

/**
 * The modes whose keys are text rather than commands, which are the ones text may be composed
 * into. In every other mode a key is a command the moment it is pressed.
 */
const TEXT_MODES = new Set(["INSERT", "COMMAND"]);

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
  statusTop: 0,
  visibleRows: 0,
  scrollTop: 0,
  lineCount: 0,
  cursorLine: 0,
};

/** What an IME is composing right now, `""` when nothing is being composed. */
let composition = "";

/**
 * Where the buffer came from and where `:w` writes it back, `null` until a file is opened.
 *
 * `{ kind: "daemon", client, path }` for a file a daemon serves, `{ kind: "local", handle, name }`
 * for one the browser opened through the File System Access API. The two differ in more than
 * where the bytes go: a daemon takes any path under the directory it serves, while the browser
 * hands over the one file that was picked and no way of naming another.
 */
let openFile = null;

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
 * The Ctrl combinations the core's grammar has a command for, lower-cased as `parse_keys`
 * reads them. Every other one is left to the browser, so that Ctrl+F, Ctrl+P and the rest go
 * on working; add to this when the grammar grows a Ctrl key.
 */
const CORE_CTRL_KEYS = new Set(["r"]);

/**
 * `text` as a key string `parse_keys` reads back as exactly those characters.
 *
 * `<` is the one character the notation spells for itself: left alone it would open a `<…>`
 * group and be read as some other key, or fail to parse and take the whole batch with it.
 */
function keysOf(text) {
  return text.replaceAll("<", "<lt>");
}

/**
 * The key notation `parse_keys` reads for `event`, or `null` for a key the core has no name
 * for — a bare modifier, an arrow, a browser shortcut — which the browser keeps.
 */
function keyNotation(event) {
  // Every key of a dead-key or IME composition belongs to the composition, the named ones
  // included: Enter and Esc confirm and abandon what is being composed, and Backspace edits it.
  // Taking any of them here would type the raw key, or leave Insert mode with a composition
  // still open in the textarea. What is composed reaches the editor on `compositionend`.
  if (event.isComposing) {
    return null;
  }
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
  if (event.key.length !== 1 || event.metaKey) {
    return null;
  }
  const literal = keysOf(event.key);
  // A key typed with Alt involved is either composed text or a shortcut, and the modifier
  // flags alone cannot tell the two apart: Firefox on Windows raises the AltGraph state for
  // plain Ctrl+Alt, and Alt+F is the browser's own menu shortcut. What does tell them apart
  // is the character itself — AltGr and macOS Option exist to type what a bare key cannot
  // (`@` on a German layout, `€` on Option), so a composed key never reads as a plain ASCII
  // letter, digit or space, while a shortcut's key is exactly that: Alt+F is a menu, Alt+Space
  // the window's system menu, and an AltGr space arrives as a distinct character (U+00A0).
  if (event.altKey || event.getModifierState("AltGraph")) {
    return /^[a-zA-Z0-9 ]$/.test(event.key) ? null : literal;
  }
  if (event.ctrlKey) {
    return CORE_CTRL_KEYS.has(event.key.toLowerCase())
      ? `<C-${event.key.toLowerCase()}>`
      : null;
  }
  return literal;
}

function handleKeys(keys) {
  const outcome = editor.handle_keys(keys);
  lastOutcome = {
    damageStart: outcome.damage_start,
    damageEnd: outcome.damage_end,
    effects: JSON.parse(outcome.effects),
  };
  draw();
  // Keys are what changes the mode, and the mode is what decides whether an IME may compose.
  syncImeFocus();
  for (const effect of lastOutcome.effects) {
    if (effect.kind === "save") {
      // Writing reaches a daemon or the file system, and neither answers within the key that
      // asked for it: what it did turns up in the report line once it is done.
      void save(effect.path ?? null);
    }
  }
  return lastOutcome;
}

/** Shows `message` under the demo, which is where opening and saving report. */
function report(message) {
  fileStatus.textContent = message;
}

/** Replaces the buffer with `text`, which is what opening a file leaves behind. */
function loadText(text) {
  editor = new WimEditor(text);
  lastOutcome = { damageStart: 0, damageEnd: 0, effects: [] };
  view = { ...view, scrollTop: 0 };
  draw({ full: true });
  // A fresh editor is in Normal mode, whatever the one it replaced was in.
  syncImeFocus();
}

/** Lets go of whatever is open, closing the connection a daemon's file was read over. */
function closeOpenFile() {
  if (openFile !== null && openFile.kind === "daemon") {
    openFile.client.close();
  }
  openFile = null;
}

/**
 * Takes the browser's focus off the file controls, so that the keys typed after a file is opened
 * are the editor's.
 *
 * Keys typed into the file controls belong to them, which is what the key handler leaves them to.
 * A button that kept the focus after it was clicked would go on taking keys from there — and the
 * Enter of a `:w` would press it again rather than save.
 */
function focusEditor() {
  if (document.activeElement instanceof HTMLElement) {
    document.activeElement.blur();
  }
  syncImeFocus();
}

/**
 * Writes the buffer out for `:w`, to `path` when the command named one and to the file that was
 * opened when it did not, which is what `null` says.
 *
 * The core does no file IO of its own — it hands back the request and the host carries it out
 * (`documents/adr/0001-daemon-fs-provider.md`) — so this is where the demo's two ways of reaching
 * a file are told apart.
 */
async function save(path) {
  if (openFile === null) {
    report("開いているファイルがありません");
    return;
  }
  const text = editor.text();
  try {
    if (openFile.kind === "daemon") {
      const destination = path ?? openFile.path;
      await openFile.client.write(destination, text);
      report(`${destination} を保存しました`);
      return;
    }
    if (path !== null) {
      // The browser hands over the one file the picker was pointed at and no way to name
      // another, so writing somewhere else would take a picker of its own — which opens on a
      // click rather than on a command.
      report("ローカルファイルでは :w にパスを指定できません");
      return;
    }
    const writable = await openFile.handle.createWritable();
    await writable.write(text);
    await writable.close();
    report(`${openFile.name} を保存しました`);
  } catch (error) {
    report(`保存できません: ${error.message}`);
  }
}

/** Opens the file the daemon form names, and keeps the connection for the saves that follow. */
async function openFromDaemon() {
  const path = daemonPath.value.trim();
  report("デーモンに接続しています");
  let client;
  try {
    client = await connect(daemonAddress.value, daemonToken.value);
  } catch (error) {
    report(`接続できません: ${error.message}`);
    return;
  }
  let content;
  try {
    content = await client.read(path);
  } catch (error) {
    client.close();
    report(`開けません: ${error.message}`);
    return;
  }
  // The connection the file was read over is the one it is written back over, so a save reaches
  // the daemon that has the file rather than whatever the form says by then.
  closeOpenFile();
  openFile = { kind: "daemon", client, path };
  loadText(content);
  focusEditor();
  report(`${path} を開きました`);
}

/** Opens a file the browser hands over, which is the one it will let the page write back. */
async function openLocalFile() {
  let handle;
  try {
    [handle] = await window.showOpenFilePicker();
  } catch (error) {
    // Closing the picker without choosing anything throws, and is not a failure to report: it
    // is someone deciding not to open a file after all.
    if (error.name !== "AbortError") {
      report(`開けません: ${error.message}`);
    }
    return;
  }
  let text;
  try {
    text = await (await handle.getFile()).text();
  } catch (error) {
    report(`開けません: ${error.message}`);
    return;
  }
  closeOpenFile();
  openFile = { kind: "local", handle, name: handle.name };
  loadText(text);
  focusEditor();
  report(`${handle.name} を開きました`);
}

/** The cells `text` draws as: one per column the core counts, carrying its display width. */
function cellsOf(text) {
  return JSON.parse(display_cells(text));
}

/** The CSS pixels `cells` occupy on a row. */
function cellsWidth(cells) {
  return cells.reduce((width, cell) => width + cell.width * view.cellWidth, 0);
}

/**
 * Points the browser's input focus at the hidden textarea in the modes whose keys are text, and
 * away from it in the ones whose keys are commands.
 *
 * Focus is what turns an IME on and off here, rather than `readonly` or `inputmode`, because it
 * is the one control that certainly stops a composition: text is only ever composed into the
 * focused editable element, so an unfocused textarea composes nothing, and a key pressed in
 * Normal mode arrives as itself and is consumed as a command. `readonly` leaves the element
 * focused and browsers disagree over whether an IME may still open on it, and `inputmode` only
 * speaks to on-screen keyboards. Nothing else on the page takes focus, and the key handler sits
 * on `window`, so the keys the demo reads are the same either way.
 */
function syncImeFocus() {
  if (TEXT_MODES.has(editor.mode())) {
    if (document.activeElement !== imeInput) {
      // Focusing scrolls the element into view by default, which would jump the page on a
      // textarea that sits wherever the cursor happens to be.
      imeInput.focus({ preventScroll: true });
    }
    return;
  }
  if (document.activeElement === imeInput) {
    imeInput.blur();
  }
}

/** Where the cursor is drawn, in CSS pixels from the top left corner of the canvas. */
function cursorPoint() {
  const commandLine = editor.command_line();
  if (commandLine !== undefined) {
    // Command-line mode types into the status line, so that is where the cursor is.
    return { x: PADDING + cellsWidth(cellsOf(commandLine)), y: view.statusTop };
  }
  const cells = cellsOf(editor.line(editor.cursor_line()));
  return {
    x: view.textLeft + cellsWidth(cells.slice(0, editor.cursor_col())),
    y: PADDING + (editor.cursor_line() - view.scrollTop) * LINE_HEIGHT,
  };
}

/**
 * Draws the composition over the canvas at the cursor, and moves the textarea under it.
 *
 * The composition is not the editor's text until the IME confirms it, so it is drawn as an
 * overlay rather than pushed through the core and taken back out again. The textarea follows
 * the cursor because the candidate window the IME opens is placed against the element it is
 * composing into.
 */
function drawComposition() {
  const point = cursorPoint();
  imeInput.style.left = `${point.x}px`;
  imeInput.style.top = `${point.y}px`;
  preedit.style.left = `${point.x}px`;
  preedit.style.top = `${point.y}px`;
  preedit.textContent = composition;
  preedit.hidden = composition === "";
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
  // A cell past the end of the line is the one an empty line's cursor sits in, one wide.
  const width =
    editor.mode() === "INSERT" ? 2 : (cells[col]?.width ?? 1) * view.cellWidth;
  context.fillStyle = COLORS.cursor;
  context.fillRect(view.textLeft + cellsWidth(cells.slice(0, col)), top, width, LINE_HEIGHT);
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
 * The lines the last batch of keys left needing a redraw: the damage the editor reported, plus
 * the rows the cursor left and landed on.
 *
 * The damage already counts the rows a deletion emptied, so a row that has fallen past the end
 * of the buffer is in it and gets the end-of-buffer filler drawn over the text it used to hold.
 */
function damagedRows() {
  const rows = new Set([view.cursorLine, editor.cursor_line()]);
  for (let line = lastOutcome.damageStart; line < lastOutcome.damageEnd; line += 1) {
    rows.add(line);
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
    statusTop,
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
  // The overlay is a DOM element rather than pixels on the canvas, so it does not follow a
  // cursor that moved, a viewport that scrolled or a resize on its own.
  drawComposition();
}

await init();
editor = new WimEditor(INITIAL_TEXT);
// The overlay draws the same glyphs at the same size as the canvas, and `main.js` is where that
// size is decided, so the two cannot drift.
preedit.style.font = fontAt(FONT_SIZE);
preedit.style.lineHeight = `${LINE_HEIGHT}px`;
draw({ full: true });

// Listening only once the editor exists is what keeps a key typed during the wasm fetch from
// reaching a demo that has nothing to type into.
window.addEventListener("keydown", (event) => {
  // The file controls are text fields and buttons of the page's own, and a key typed into one
  // of them is theirs: taking it here would leave an address that cannot be typed.
  if (event.target instanceof Element && event.target.closest("#file-access") !== null) {
    return;
  }
  const keys = keyNotation(event);
  if (keys === null) {
    return;
  }
  event.preventDefault();
  handleKeys(keys);
});

window.addEventListener("resize", () => draw());

// An IME composes into the textarea; the demo only watches, and takes the text once the IME
// says it is confirmed. Until then what is on screen is the overlay, and the buffer is untouched
// — which is what lets a composition be abandoned without the editor ever having heard of it.
imeInput.addEventListener("compositionstart", (event) => {
  // `data` is `null` on a start that carries no text, which is every one of them here: the
  // textarea is emptied after each composition, so there is never anything to compose over.
  composition = event.data ?? "";
  drawComposition();
});

imeInput.addEventListener("compositionupdate", (event) => {
  composition = event.data ?? "";
  drawComposition();
});

imeInput.addEventListener("compositionend", (event) => {
  composition = "";
  drawComposition();
  // The textarea is only somewhere for the IME to work in. What it is left holding is the
  // editor's now, so it goes in as keys and the textarea starts the next composition empty.
  imeInput.value = "";
  // A composition that was abandoned rather than confirmed ends with no text, and there is
  // nothing to type: the buffer never held any of it.
  const text = event.data ?? "";
  if (text !== "") {
    handleKeys(keysOf(text));
  }
});

// Text can reach a focused textarea without a composition — a paste, a drop, an autofill — and
// none of those are wired through the core yet. Dropping it keeps the textarea empty, so that
// the leftovers cannot turn up in front of the next composition.
imeInput.addEventListener("input", (event) => {
  if (!event.isComposing) {
    imeInput.value = "";
  }
});

// A click would otherwise move focus off the textarea and leave Insert mode without an IME.
// The canvas holds no selectable text of its own, so nothing is lost by keeping the focus.
canvas.addEventListener("pointerdown", (event) => {
  event.preventDefault();
  syncImeFocus();
});

daemonForm.addEventListener("submit", (event) => {
  // The form is the page's own: it opens a file over a WebSocket rather than navigating.
  event.preventDefault();
  void openFromDaemon();
});

localButton.addEventListener("click", () => void openLocalFile());

if (window.showOpenFilePicker === undefined) {
  // Firefox and Safari have no File System Access API, and without it a page can read a file
  // through an `<input type="file">` but has nowhere to write it back to.
  localButton.disabled = true;
  localButton.title = "このブラウザは File System Access API に対応していません";
}

// The handle the E2E run drives and inspects the demo through.
window.wimDemo = {
  sendKeys: handleKeys,
  /** Replaces the buffer, which is how the E2E run gets one taller than the viewport. */
  load: loadText,
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
    ime: {
      /** What an IME is composing, which the overlay shows and the buffer does not hold yet. */
      composition,
      /** Whether the textarea an IME composes into is the focused element. */
      focused: document.activeElement === imeInput,
      cursor: cursorPoint(),
    },
    layout: {
      cellWidth: view.cellWidth,
      textLeft: view.textLeft,
      lineHeight: LINE_HEIGHT,
      padding: PADDING,
    },
  }),
};
