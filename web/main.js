// The demo host: it turns browser key events into the key notation wim-core reads, and draws
// the state the editor hands back onto a canvas.
//
// Every key redraws the whole canvas. The editor also reports the lines it changed, which the
// glyph atlas and the damage-driven redraw of the next phase will use; until then the damage
// is only carried in the state the E2E run reads.

import init, { WimEditor } from "./pkg/wim_wasm.js";

const INITIAL_TEXT = `wim is a Vim-grammar editor, not a Vim clone.
The core is one pure Rust crate, compiled to Wasm for this page.

Type i to insert, Esc to go back, dd to delete this line.`;

/** Font size in CSS pixels. The cell size is measured from it, so the two never drift. */
const FONT_SIZE = 16;
const LINE_HEIGHT = 22;
const PADDING = 12;
/** Height of the canvas in CSS pixels, which is what the `height` attribute in the page says. */
const HEIGHT = 440;

const FONT = `${FONT_SIZE}px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace`;

const canvas = document.querySelector("#screen");
const context = canvas.getContext("2d");

/** The editor, which cannot be built before the wasm module is initialised. */
let editor;

/** What the last batch of keys did, which the demo shows in its mode line. */
let lastOutcome = { damageStart: 0, damageEnd: 0, effects: [] };

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

function modeLine() {
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

function draw() {
  // The backing store is in device pixels and the drawing is in CSS pixels, so the text stays
  // sharp on a display that has more of the former than the latter.
  const scale = window.devicePixelRatio || 1;
  const width = canvas.clientWidth;
  canvas.width = width * scale;
  canvas.height = HEIGHT * scale;
  // Growing the backing store grows the element unless the CSS height pins it down.
  canvas.style.height = `${HEIGHT}px`;
  context.setTransform(scale, 0, 0, scale, 0, 0);

  context.fillStyle = "#12141a";
  context.fillRect(0, 0, width, HEIGHT);

  context.font = FONT;
  context.textBaseline = "top";
  // One cell is the advance of a character in the monospace font, so the cursor lands on the
  // character the core says it is on whatever the font the browser picked.
  const cellWidth = context.measureText("M").width;

  // The cursor goes under the text so that the character it sits on stays readable.
  const cursorX = PADDING + editor.cursor_col() * cellWidth;
  const cursorY = PADDING + editor.cursor_line() * LINE_HEIGHT;
  context.fillStyle = "#5c9cf5";
  context.fillRect(cursorX, cursorY, editor.mode() === "INSERT" ? 2 : cellWidth, LINE_HEIGHT);

  context.fillStyle = "#d8dee9";
  for (let line = 0; line < editor.line_count(); line += 1) {
    context.fillText(editor.line(line), PADDING, PADDING + line * LINE_HEIGHT);
  }

  context.fillStyle = "#6a7383";
  context.fillText(modeLine(), PADDING, HEIGHT - PADDING - LINE_HEIGHT);
}

await init();
editor = new WimEditor(INITIAL_TEXT);
draw();

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

window.addEventListener("resize", draw);

// The handle the E2E run drives and inspects the demo through.
window.wimDemo = {
  sendKeys: handleKeys,
  state: () => ({
    text: editor.text(),
    lines: Array.from({ length: editor.line_count() }, (_, line) => editor.line(line)),
    cursor: { line: editor.cursor_line(), col: editor.cursor_col() },
    mode: editor.mode(),
    commandLine: editor.command_line() ?? null,
    damage: { start: lastOutcome.damageStart, end: lastOutcome.damageEnd },
    effects: lastOutcome.effects,
  }),
};
