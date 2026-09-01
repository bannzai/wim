// The demo host: it turns browser key events into the key notation wim-core reads, and draws
// the state the editor hands back onto a canvas.
//
// Drawing follows documents/PROJECT.md. Glyphs are baked once into an offscreen atlas and
// blitted from there rather than laid out again on every frame, and a key redraws only the
// lines the editor reports as damaged plus the rows the cursor left and landed on. Anything
// that moves every row — a scroll, a resize, a new buffer — redraws the lot.

import init, { WimEditor, display_cells } from "./pkg/wim_wasm.js";
import { connect } from "./daemon.js";
import { createHighlighter, languageOf } from "./highlight.js";
import { loadPlugins } from "./plugins.js";
import { loadConfig } from "./config.js";

const INITIAL_TEXT = `wim is a Vim-grammar editor, not a Vim clone.
The core is one pure Rust crate, compiled to Wasm for this page.

Type i to insert, Esc to go back, dd to delete this line.
全角も 1 桁ではなく 2 桁ぶんの幅で描く。`;

/**
 * Buffers the sample buttons load, under the name that says which language they are in.
 *
 * Highlighting is otherwise only seen on a file that was opened, and neither way of opening one
 * is open to everybody — a daemon has to be running, and the browser's picker is Chromium's — so
 * the demo carries a buffer of each language it highlights.
 */
const SAMPLES = {
  "sample.rs": `// Rust, highlighted by tree-sitter.
use std::fmt::Write as _;

#[derive(Debug, Clone)]
pub struct Buffer {
    lines: Vec<String>,
}

impl Buffer {
    pub fn joined(&self) -> String {
        let mut text = String::new();
        for (number, line) in self.lines.iter().enumerate() {
            writeln!(text, "{number}: {line}").unwrap();
        }
        text
    }
}
`,
  "sample.md": `# Markdown, highlighted by tree-sitter

wim reads the buffer's *extension* to pick a grammar.

- \`.rs\` loads tree-sitter-rust
- \`.md\` loads tree-sitter-markdown

\`\`\`rust
fn main() {
    println!("the fence is drawn as one run");
}
\`\`\`

See <https://github.com/bannzai/wim> for the rest.
`,
};

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
 * Thickness in CSS pixels of the line drawn under an unconfirmed composition. Two pixels is the
 * thinnest that stays visible on a display with one device pixel per CSS pixel and still clears
 * the descenders of a 16px font on a 22px row.
 */
const PREEDIT_UNDERLINE = 2;

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

/** The form that opens a file through a daemon, and the fields naming which one. */
const daemonForm = document.querySelector("#daemon-form");
const daemonAddress = document.querySelector("#daemon-address");
const daemonToken = document.querySelector("#daemon-token");
const daemonPath = document.querySelector("#daemon-path");

/** The button that opens a file the browser itself hands over and writes back. */
const localButton = document.querySelector("#local-open");

/** The buttons that put one of `SAMPLES` in the buffer, keyed by the name they load it under. */
const sampleButtons = {
  "sample.rs": document.querySelector("#sample-rust"),
  "sample.md": document.querySelector("#sample-markdown"),
};

/** Where opening and saving report what they did, under the file controls. */
const fileStatus = document.querySelector("#file-status");

/** The line listing the commands the loaded plugins published, and the one a run reports on. */
const pluginCommandList = document.querySelector("#plugin-commands");
const pluginStatus = document.querySelector("#plugin-status");

/** The line listing the autocmds the config declared, and the one a handler reports on. */
const autocmdList = document.querySelector("#autocmd-list");
const autocmdStatus = document.querySelector("#autocmd-status");

/** The config the demo binds its autocmds from, served next to the page. */
const CONFIG = "./wim.jsonc";

/**
 * The modes whose keys are text rather than commands, which are the ones text may be composed
 * into. In every other mode a key is a command the moment it is pressed.
 */
const TEXT_MODES = new Set(["INSERT", "COMMAND"]);

/** The editor, which cannot be built before the wasm module is initialised. */
let editor;

/**
 * The name the buffer is under, which is what a plugin is given as the name of the snapshot and
 * what the language is picked from. Empty for a buffer that came from no file, which is what the
 * demo starts on.
 */
let bufferName = "";

/**
 * The commands the loaded plugins published, keyed by the name `:name` runs them under. Empty
 * until the modules have been fetched, and on a demo served without any.
 */
let pluginCommands = new Map();

/**
 * The plugins the build transpiled, keyed by the name an autocmd names them by. A command is
 * looked up in `pluginCommands`; this is what an event is delivered over.
 */
let plugins = new Map();

/**
 * The autocmds the config declared, in the order they are written. Empty until `wim.jsonc` has
 * been fetched, and on a demo served without one.
 */
let autocmds = [];

/**
 * Whether a handler is running, which is what keeps a handler that edits the buffer from being
 * run again by the event its own edit reports. Vim's autocmds nest only when they are asked to;
 * here they never do, and the native host does the same (`crates/wim/src/edit.rs`).
 */
let inHandler = false;

/** What the handlers of the last batch of keys did, which the E2E run reads. */
let lastAutocmds = [];

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
 * The tree-sitter highlighter over the buffer, `null` for a buffer in no language the demo has a
 * grammar for — which is what the demo starts on, and what a file with an unknown extension gets.
 */
let highlighter = null;

/**
 * How many languages have been asked for, which is what tells a grammar that has finished loading
 * whether it is still the one the buffer is in. Fetching one takes a round trip, and a second file
 * may well be opened before the first one's grammar has arrived.
 */
let languageGeneration = 0;

/** The rows the last batch of keys changed the highlighting of, which are redrawn with the damage. */
let highlightDamage = new Set();

/**
 * Where the buffer came from and where `:w` writes it back, `null` until a file is opened.
 *
 * `{ kind: "daemon", client, path, newline }` for a file a daemon serves,
 * `{ kind: "local", handle, name, newline }` for one the browser opened through the File System
 * Access API. The two differ in more than where the bytes go: a daemon takes any path under the
 * directory it serves, while the browser hands over the one file that was picked and no way of
 * naming another.
 */
let openFile = null;

/**
 * How many opens have been asked for, which is what tells a completion whether it is still the one
 * being waited for. An open counts from the point it names a file: the daemon form names one when
 * it is submitted, while the browser's picker does not until it hands one over.
 *
 * Neither way of opening a file answers within the click that asked for it, and nothing stops a
 * second one from being started while the first is still in the air. Whichever finishes last
 * would otherwise become the open file whatever order they were asked for in, and a stale one
 * finishing after a newer one would close the connection the newer file is read and written over.
 */
let openGeneration = 0;

/**
 * The save in flight, which the next one waits for.
 *
 * A writable stream per save is a silo of its own: two of them opened over one file may close in
 * the order they finish rather than the order they were started, and the earlier buffer would
 * land on the file after the later one. Chaining them keeps the last `:w` the last write.
 */
let writing = Promise.resolve();

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
  //
  // Safari raises the keydown that confirms a composition — Enter, most of the time — with
  // `isComposing` already false and `compositionend` not yet fired, and the only thing left
  // marking it is the key code 229 that UI Events reserves for a key the IME is processing
  // (https://www.w3.org/TR/uievents/#determine-keydown-keyup-keyCode). Reading that one as
  // `<CR>` would split the line or run the Ex command while the IME is still committing.
  if (event.isComposing || event.keyCode === 229) {
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
  const invocation = pluginInvocation(keys);
  if (invocation !== null) {
    // The core has no `:upcase`, and letting it read the line would answer with its own "not an
    // editor command". `<Esc>` drops the line instead, leaving the editor where a command that
    // ran would have left it — in Normal mode with the line gone.
    editor.handle_keys("<Esc>");
    lastOutcome = { damageStart: 0, damageEnd: 0, effects: [] };
    runPlugin(invocation);
    // A plugin may rewrite the whole buffer, and what it did is not in the core's damage.
    draw({ full: true });
    syncImeFocus();
    return lastOutcome;
  }
  const outcome = editor.handle_keys(keys);
  lastOutcome = {
    damageStart: outcome.damage_start,
    damageEnd: outcome.damage_end,
    effects: JSON.parse(outcome.effects),
  };
  // Reparsing before the frame is what lets the rows whose colours changed be drawn with the rows
  // whose text did. The viewport of the frame just gone is the one to look at: a key that moves it
  // redraws everything anyway.
  highlightDamage =
    highlighter === null
      ? new Set()
      : highlighter.update(
          editor.text(),
          Array.from({ length: view.visibleRows }, (_, row) => view.scrollTop + row),
        );
  draw();
  // Keys are what changes the mode, and the mode is what decides whether an IME may compose.
  syncImeFocus();
  carryOut(lastOutcome.effects);
  return lastOutcome;
}

/**
 * Carries out what the core handed back, in the order it handed it back.
 *
 * The order is what puts a `buffer-write` handler in front of the write it belongs to: the core
 * reports the event ahead of the request, so whatever the handler edits is in the buffer by the
 * time the text to write is read out of it (`crates/wim-core/src/effect.rs`).
 */
function carryOut(effects) {
  lastAutocmds = [];
  // A handler that rewrites the buffer replaces the editor, which starts a new outcome; what
  // this batch of keys did is the one to report, so it is put back afterwards.
  const outcome = lastOutcome;
  for (const effect of effects) {
    if (effect.kind === "event") {
      dispatch(effect.name, effect.payload);
    }
    if (effect.kind === "save") {
      // Writing reaches a daemon or the file system, and neither answers within the key that
      // asked for it: what it did turns up in the report line once it is done.
      void save(effect.path ?? null);
    }
  }
  lastOutcome = outcome;
}

/** Runs every autocmd bound to the event `name`, and redraws what they changed. */
function dispatch(name, payload) {
  if (inHandler) {
    return;
  }
  const bound = autocmds.filter((autocmd) => autocmd.event === name);
  if (bound.length === 0) {
    return;
  }
  inHandler = true;
  try {
    for (const { handler } of bound) {
      lastAutocmds.push(`${name} ${runHandler(name, payload, handler)}`);
    }
  } finally {
    inHandler = false;
  }
  reportAutocmd(lastAutocmds.join(" / "));
  // A handler may have rewritten the buffer, and what it did is not in the damage the keys
  // reported.
  draw({ full: true });
  syncImeFocus();
}

/** Runs one handler and says what it did, for the line under the list of them. */
function runHandler(name, payload, handler) {
  try {
    switch (handler.kind) {
      case "ex":
        // The keys are the characters of the line, so a `<` in a command is typed rather than
        // read as the start of a key name. `wim edit` types the same characters natively.
        typeAtEditor(`:${handler.command.replaceAll("<", "<lt>")}<CR>`);
        return `ex: ${handler.command}`;
      case "keys":
        typeAtEditor(handler.keys);
        return `keys: ${handler.keys}`;
      default:
        return `plugin ${handler.plugin}: ${callPlugin(name, payload, handler.plugin)}`;
    }
  } catch (error) {
    return `${handler.kind} が失敗しました: ${error.message}`;
  }
}

/**
 * Types the keys of a handler at the editor and carries out what they asked for.
 *
 * The two `<Esc>`s are what closes a command a handler left half-typed, the way `:norm` closes
 * the keys of a line; without them the keys typed next would be read as the rest of it.
 */
function typeAtEditor(keys) {
  const outcome = editor.handle_keys(`${keys}<Esc><Esc>`);
  const effects = JSON.parse(outcome.effects);
  const failed = effects.find((effect) => effect.kind === "error");
  if (failed !== undefined) {
    throw new Error(failed.message);
  }
  // The events these raise are the handler's own doing, and `inHandler` is what stops them from
  // running it again; a `:w` inside a handler still writes.
  for (const effect of effects) {
    if (effect.kind === "save") {
      void save(effect.path ?? null);
    }
  }
}

/**
 * Gives the event to a plugin and applies the edit it answers with.
 *
 * The buffer crosses by value and the answer comes back by value, which is the whole of what the
 * ABI lets a plugin touch: the edit is applied here, by the host.
 */
function callPlugin(name, payload, plugin) {
  const loaded = plugins.get(plugin);
  if (loaded === undefined) {
    throw new Error(`${plugin} というプラグインは読み込まれていません`);
  }
  if (!loaded.subscriptions.includes(name)) {
    // The ABI has the host deliver nothing a plugin did not subscribe to.
    throw new Error(`${plugin} は ${name} を購読していません`);
  }
  return applyEdit(
    loaded.onEvent(
      { name, payload },
      {
        name: bufferName,
        text: editor.text(),
        cursor: { line: editor.cursor_line(), column: editor.cursor_col() },
      },
    ),
  );
}

/** Shows `message` under the demo, which is where opening and saving report. */
function report(message) {
  fileStatus.textContent = message;
}

/** Shows what running a plugin command did, on the line under the list of them. */
function reportPlugin(message) {
  pluginStatus.textContent = message;
}

/** Shows what the autocmds that just ran did, on the line under the list of them. */
function reportAutocmd(message) {
  autocmdStatus.textContent = message;
}

/**
 * The plugin command the batch `keys` submits, `null` when it submits none.
 *
 * A plugin's command is taken before the core sees it rather than after: the core has no such
 * command and would answer with an error whose message carries the name but not the arguments
 * that followed it, and `:upcase x` has to reach the plugin as an argument for the plugin to
 * refuse it the way it does natively.
 *
 * A command line is only ever submitted by one `<CR>` on its own here — a key press is handled as
 * it arrives, and what an IME confirms goes in as text — so that is the one batch to look at.
 */
function pluginInvocation(keys) {
  if (keys !== "<CR>") {
    return null;
  }
  const line = editor.command_line();
  if (line === undefined || !line.startsWith(":")) {
    return null;
  }
  const [name, ...args] = line.slice(1).trim().split(/\s+/);
  const command = pluginCommands.get(name);
  return command === undefined ? null : { command, args };
}

/**
 * Runs a plugin command over the buffer as it stands and applies what comes back.
 *
 * The buffer goes over by value and the answer comes back by value, which is the whole of what
 * the ABI lets a plugin touch (`wit/README.md`): the edit is applied here, by the host.
 */
function runPlugin({ command, args }) {
  let edit;
  try {
    edit = command.run(args, {
      name: bufferName,
      text: editor.text(),
      cursor: { line: editor.cursor_line(), column: editor.cursor_col() },
    });
  } catch (error) {
    // What the plugin refuses arrives as the `result<edit, string>` error half, in its wording.
    reportPlugin(`:${command.name} が失敗しました: ${error.message}`);
    return;
  }
  reportPlugin(`:${command.name}: ${applyEdit(edit)}`);
}

/** Carries out one `edit` of the ABI and says what it did (`wit/plugin.wit`). */
function applyEdit(edit) {
  switch (edit.tag) {
    case "replace-all":
      void loadText(edit.val, bufferName);
      return "バッファを書き換えました";
    case "replace-lines": {
      // Each line keeps its own newline, so a buffer whose last line has none stays that way
      // unless the replacement is the piece that lands at the end. An empty buffer is the one
      // empty line the editor shows, which is the line a `{ start: 0, end: 1 }` edit means.
      // `wim plugin run` splices the same way (`crates/wim/src/plugin.rs`).
      const text = editor.text();
      const lines = text === "" ? [""] : text.split(/(?<=\n)/);
      const { start, end } = edit.val;
      if (start > end || end > lines.length) {
        return `${start}..${end} 行は ${lines.length} 行のバッファにありません`;
      }
      void loadText(
        lines.slice(0, start).join("") + edit.val.text + lines.slice(end).join(""),
        bufferName,
      );
      // Counted from 1, the way the gutter numbers the row this landed on.
      return `${start + 1} 行目からを書き換えました`;
    }
    case "message":
      return edit.val;
    case "noop":
      return "何も変えませんでした";
    default:
      // A tag from an ABI this host does not know, which the version check should have stopped.
      return `知らない edit が返りました: ${edit.tag}`;
  }
}

/**
 * Fetches the transpiled plugins and registers what they publish, which is the browser's half of
 * what `Plugin::from_file` and `list_commands` do natively.
 */
async function startPlugins() {
  const { commands, plugins: loaded, failures } = await loadPlugins();
  pluginCommands = commands;
  plugins = loaded;
  const published = [...commands.values()].map(
    (command) => `:${command.name} — ${command.description}`,
  );
  pluginCommandList.textContent =
    published.length === 0
      ? "プラグインは読み込まれていません"
      : `プラグインのコマンド ${published.join(" / ")}`;
  if (failures.length > 0) {
    reportPlugin(`読み込めないプラグインがあります: ${failures.join(" / ")}`);
  }
}

/**
 * Reads `wim.jsonc` and registers the autocmds it declares, which is the browser's half of what
 * `wim edit` does with --config (`documents/CONFIG.md`).
 *
 * A handler of kind `plugin` is checked against what that plugin subscribed to once the plugins
 * are in, so that a binding which could never fire is reported where the config is read rather
 * than by never running.
 */
async function startAutocmds() {
  const config = await loadConfig(CONFIG);
  if (config.error !== null) {
    autocmdList.textContent = `${CONFIG} を読めません: ${config.error}`;
    return;
  }
  autocmds = config.autocmds;
  const declared = autocmds.map((autocmd) => `${autocmd.event} → ${describe(autocmd.handler)}`);
  autocmdList.textContent =
    declared.length === 0
      ? "autocmd は設定されていません"
      : `autocmd ${declared.join(" / ")}`;
  await pluginsStarted;
  const unreachable = autocmds
    .filter((autocmd) => autocmd.handler.kind === "plugin")
    .filter(
      (autocmd) =>
        !plugins.get(autocmd.handler.plugin)?.subscriptions.includes(autocmd.event),
    )
    .map((autocmd) => `${autocmd.handler.plugin} → ${autocmd.event}`);
  if (unreachable.length > 0) {
    reportAutocmd(`配送されない autocmd があります: ${unreachable.join(" / ")}`);
  }
}

/** One handler, as the list of declared autocmds shows it. */
function describe(handler) {
  switch (handler.kind) {
    case "ex":
      return `:${handler.command}`;
    case "keys":
      return handler.keys;
    default:
      return handler.plugin;
  }
}

/**
 * Replaces the buffer with `text`, which is what opening a file leaves behind, and starts
 * highlighting it as the language `name` is written in.
 *
 * The buffer is on screen before the grammar is: it is fetched over the network and the text is
 * readable, and editable, while that is in the air. The answer is the grammar arriving, which is
 * what the E2E run waits on.
 */
function loadText(text, name = "") {
  editor = new WimEditor(text);
  bufferName = name;
  lastOutcome = { damageStart: 0, damageEnd: 0, effects: [] };
  view = { ...view, scrollTop: 0 };
  const highlighted = setLanguage(name);
  draw({ full: true });
  // A fresh editor is in Normal mode, whatever the one it replaced was in.
  syncImeFocus();
  return highlighted;
}

/**
 * Highlights the buffer as the language a file called `name` is written in, and stops highlighting
 * it at all when that is a name no grammar is loaded for.
 */
async function setLanguage(name) {
  const generation = (languageGeneration += 1);
  highlighter?.close();
  highlighter = null;
  highlightDamage = new Set();
  const language = languageOf(name);
  if (language === null) {
    return;
  }
  let started;
  try {
    started = await createHighlighter(language, editor.text());
  } catch (error) {
    if (generation === languageGeneration) {
      report(`${language} のハイライトを読み込めません: ${error.message}`);
    }
    return;
  }
  if (generation !== languageGeneration) {
    // Another buffer is being highlighted by now, and this grammar has nothing left to parse.
    started.close();
    return;
  }
  highlighter = started;
  // Keys typed while the grammar was in the air went into the buffer and not into the tree, which
  // was parsed from the text as it stood when the grammar was asked for.
  highlighter.update(editor.text(), []);
  draw({ full: true });
}

/**
 * The line separator a file opened as `text` is written back with.
 *
 * The core edits LF text, so a CRLF file is normalized on the way in and restored on the way out,
 * which is what the `vimacro` host does with the same core. A file holding both endings counts as
 * CRLF there and here, so a save leaves one ending rather than the mixture it was opened with.
 */
function newlineOf(text) {
  return text.includes("\r\n") ? "\r\n" : "\n";
}

/** `text` with its line endings back as `newline`, which is how the file it came from reads. */
function withNewline(text, newline) {
  return newline === "\r\n" ? text.replaceAll("\n", "\r\n") : text;
}

/**
 * Lets go of whatever is open, closing the connection a daemon's file was read over once the
 * saves already queued for it have gone out.
 *
 * A `:w` takes its snapshot when it is typed and then waits its turn on `writing`, so a save
 * still queued when a file is let go belongs to that file and is written over its connection.
 * Closing where this stands would leave those saves with a socket that is already going away and
 * the daemon would never hear the last of the edits, so the close goes on the end of the same
 * chain instead — after the writes it must not overtake, and before nothing.
 *
 * The open that let go does not wait for that. The file it is opening is reached over a
 * connection of its own and nothing it does depends on the old one being down, while waiting
 * would hold a new file behind a save the daemon is slow to answer. Saves typed over the new file
 * queue behind the close, which is what the one chain already did for saves over two files.
 */
function closeOpenFile() {
  if (openFile === null) {
    return;
  }
  const closing = openFile;
  openFile = null;
  if (closing.kind !== "daemon") {
    return;
  }
  // `writeOut` reports what it cannot write rather than throwing, so the chain this is put on
  // stays fulfilled and the close is reached whatever the saves before it did.
  writing = writing.then(() => closing.client.close());
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
 *
 * The buffer is taken here and the writing waits for the save before it, so that saves land in
 * the order they were typed and each one puts the text `:w` was typed over on the file.
 */
function save(path) {
  if (openFile === null) {
    report("開いているファイルがありません");
    return;
  }
  // The buffer is read here rather than in the write itself, so that a `:w` writes the text that
  // was on screen when it was typed even when it waits for an earlier save to finish first.
  const file = openFile;
  const text = withNewline(editor.text(), file.newline);
  writing = writing.then(async () => {
    // Whether a write happened is the host's to know, so `buffer-write-post` is raised here
    // rather than by the core, and only once the bytes are on the file.
    if (await writeOut(file, path, text)) {
      dispatch("buffer-write-post", "");
    }
  });
}

/**
 * Puts `text` on `file`, to `path` when the `:w` that asked for it named one, and says whether
 * the bytes landed there.
 */
async function writeOut(file, path, text) {
  try {
    if (file.kind === "daemon") {
      const destination = path ?? file.path;
      await file.client.write(destination, text);
      report(`${destination} を保存しました`);
      return true;
    }
    if (path !== null) {
      // The browser hands over the one file the picker was pointed at and no way to name
      // another, so writing somewhere else would take a picker of its own — which opens on a
      // click rather than on a command.
      report("ローカルファイルでは :w にパスを指定できません");
      return false;
    }
    const writable = await file.handle.createWritable();
    await writable.write(text);
    await writable.close();
    report(`${file.name} を保存しました`);
    return true;
  } catch (error) {
    report(`保存できません: ${error.message}`);
    return false;
  }
}

/** Opens the file the daemon form names, and keeps the connection for the saves that follow. */
async function openFromDaemon() {
  const generation = (openGeneration += 1);
  const path = daemonPath.value.trim();
  report("デーモンに接続しています");
  let client;
  try {
    client = await connect(daemonAddress.value, daemonToken.value);
  } catch (error) {
    if (generation === openGeneration) {
      report(`接続できません: ${error.message}`);
    }
    return;
  }
  if (generation !== openGeneration) {
    // Another open was asked for while this one was connecting, and it is the one being waited
    // for: this connection has nothing left to read for and is dropped where it stands.
    client.close();
    return;
  }
  let content;
  try {
    content = await client.read(path);
  } catch (error) {
    client.close();
    if (generation === openGeneration) {
      report(`開けません: ${error.message}`);
    }
    return;
  }
  if (generation !== openGeneration) {
    client.close();
    return;
  }
  // The connection the file was read over is the one it is written back over, so a save reaches
  // the daemon that has the file rather than whatever the form says by then.
  closeOpenFile();
  openFile = { kind: "daemon", client, path, newline: newlineOf(content) };
  void loadText(content.replaceAll("\r\n", "\n"), path);
  focusEditor();
  report(`${path} を開きました`);
}

/** Opens a file the browser hands over, which is the one it will let the page write back. */
async function openLocalFile() {
  // Nothing is being opened until the picker hands a file over: it is the browser's own window
  // and it may be closed again without choosing anything. Taking a generation before it answers
  // would make that closing count as a newer open and throw away the one already in the air —
  // which would leave the file that was asked for unopened and its report the last thing said.
  const started = openGeneration;
  let handle;
  try {
    [handle] = await window.showOpenFilePicker();
  } catch (error) {
    // Closing the picker without choosing anything throws, and is not a failure to report: it
    // is someone deciding not to open a file after all.
    if (error.name !== "AbortError" && started === openGeneration) {
      report(`開けません: ${error.message}`);
    }
    return;
  }
  // A file has been picked, so this is an open now and the newest one: what was in the air before
  // it, picked or served, is no longer what is being waited for.
  const generation = (openGeneration += 1);
  let text;
  try {
    const bytes = await (await handle.getFile()).arrayBuffer();
    // The core edits text, and the daemon refuses a file that is not UTF-8 for that reason. A
    // lossy decode here would put replacement characters in the buffer and a `:w` would write
    // them over the bytes they stand for, so a file that does not decode is not opened at all.
    text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch (error) {
    if (generation !== openGeneration) {
      return;
    }
    report(
      error instanceof TypeError
        ? "UTF-8 ではないため開けません"
        : `開けません: ${error.message}`,
    );
    return;
  }
  if (generation !== openGeneration) {
    return;
  }
  closeOpenFile();
  openFile = { kind: "local", handle, name: handle.name, newline: newlineOf(text) };
  void loadText(text.replaceAll("\r\n", "\n"), handle.name);
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
 * Moves the textarea an IME composes into under the cursor, because the candidate window the IME
 * opens is placed against the element it is composing into.
 */
function placeImeInput() {
  const point = cursorPoint();
  imeInput.style.left = `${point.x}px`;
  imeInput.style.top = `${point.y}px`;
}

/**
 * The cells of what an IME is composing into a buffer line, empty when there is no composition
 * to draw there.
 *
 * The composition is not the editor's text until the IME confirms it, so it never goes through
 * the core: it is spliced into the row the cursor is on at drawing time and taken back out when
 * the IME is done. Command-line mode composes into the status line instead, which is drawn from
 * `statusText` rather than from a buffer line.
 */
function composingCells() {
  if (composition === "" || editor.command_line() !== undefined) {
    return [];
  }
  return cellsOf(composition);
}

/**
 * Underlines `width` CSS pixels of the row starting at `top`, which is how Vim and an IME's own
 * inline mode mark the text that is not confirmed yet.
 */
function drawPreeditUnderline(left, top, width) {
  context.fillStyle = COLORS.cursor;
  context.fillRect(left, top + LINE_HEIGHT - PREEDIT_UNDERLINE, width, PREEDIT_UNDERLINE);
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

/**
 * Draws `cells` from `x` rightwards, each over as many cells as its width says, in `color` unless
 * highlighting gave the cell one of its own.
 */
function drawCells(cells, x, y, color) {
  let left = x;
  for (const cell of cells) {
    drawGlyph(cell.text, cell.width, cell.color ?? color, left, y);
    left += cell.width * view.cellWidth;
  }
}

/**
 * `cells` with the colour the highlighting draws each of them in, left alone where the buffer is
 * in no language or the row has nothing coloured on it.
 *
 * A run is a range of UTF-16 units, which is what tree-sitter counts columns in, while a cell is a
 * grapheme cluster and may be several of them: a cell takes the colour of the run its first unit
 * falls in, because a run that started inside a cluster would have no glyph of its own to colour.
 */
function colorCells(cells, line) {
  if (highlighter === null) {
    return cells;
  }
  const runs = highlighter.rowRuns(line);
  let column = 0;
  let index = 0;
  for (const cell of cells) {
    while (index < runs.length && runs[index].end <= column) {
      index += 1;
    }
    if (index < runs.length && runs[index].start <= column) {
      cell.color = runs[index].color;
    }
    column += cell.text.length;
  }
  return cells;
}

/** Draws the cursor on column `col` of `cells`, under the text so the character stays readable. */
function drawCursor(cells, col, top) {
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

  const cells = colorCells(cellsOf(editor.line(line)), line);
  if (line !== view.cursorLine) {
    drawCells(cells, view.textLeft, top, COLORS.text);
    return;
  }
  // A composition is drawn as part of the row rather than over it, so the text after the cursor
  // moves right by the width of what is being composed — where it lands once the composition is
  // confirmed — instead of sitting hidden underneath it.
  const col = editor.cursor_col();
  const preedit = composingCells();
  const row = [...cells.slice(0, col), ...preedit, ...cells.slice(col)];
  // The caret goes after what has been composed so far, which is where the next character lands.
  drawCursor(row, col + preedit.length, top);
  drawCells(row, view.textLeft, top, COLORS.text);
  if (preedit.length > 0) {
    drawPreeditUnderline(
      view.textLeft + cellsWidth(row.slice(0, col)),
      top,
      cellsWidth(preedit),
    );
  }
}

function drawStatusLine(top) {
  context.fillStyle = COLORS.background;
  context.fillRect(0, top, view.width, LINE_HEIGHT);
  const cells = cellsOf(statusText());
  // Command-line mode types into the status line, so a composition open over it is drawn at the
  // end of the command line, which is where its cursor is.
  const preedit =
    composition !== "" && editor.command_line() !== undefined ? cellsOf(composition) : [];
  drawCells([...cells, ...preedit], PADDING, top, COLORS.muted);
  if (preedit.length > 0) {
    drawPreeditUnderline(PADDING + cellsWidth(cells), top, cellsWidth(preedit));
  }
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
 * The lines the last batch of keys left needing a redraw: the damage the editor reported, the rows
 * the cursor left and landed on, and the rows the reparse gave different colours to.
 *
 * The last of those is not in the editor's damage and cannot be: the core knows nothing about
 * languages, so a quote typed on one line damages that line while turning every line under it into
 * a string.
 *
 * The damage already counts the rows a deletion emptied, so a row that has fallen past the end
 * of the buffer is in it and gets the end-of-buffer filler drawn over the text it used to hold.
 */
function damagedRows() {
  const rows = new Set([view.cursorLine, editor.cursor_line(), ...highlightDamage]);
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
  // The textarea is a DOM element rather than pixels on the canvas, so it does not follow a
  // cursor that moved, a viewport that scrolled or a resize on its own.
  placeImeInput();
}

await init();
editor = new WimEditor(INITIAL_TEXT);
draw({ full: true });

// The plugins are fetched over the network and the editor is usable while that is in the air, the
// way a grammar is. The answer is what the E2E run waits on before it types a plugin's command.
const pluginsStarted = startPlugins();

// The autocmds are read out of a file the same way, and a demo served without one binds nothing.
// The answer is what the E2E run waits on before it types a key that raises an event.
const autocmdsStarted = startAutocmds();

// Listening only once the editor exists is what keeps a key typed during the wasm fetch from
// reaching a demo that has nothing to type into.
window.addEventListener("keydown", (event) => {
  // The file controls and the sample buttons are the page's own, and a key typed into one of them
  // is theirs: taking it here would leave an address that cannot be typed, and the Enter or Space
  // that presses a button someone reached with Tab would be typed into the buffer as well.
  if (
    event.target instanceof Element &&
    event.target.closest("#file-access, #samples") !== null
  ) {
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

// An IME composes into the textarea; the demo only watches, and takes the text once the IME says
// it is confirmed. Until then what is on screen is a row drawn with the composition spliced into
// it, and the buffer is untouched — which is what lets a composition be abandoned without the
// editor ever having heard of it.
imeInput.addEventListener("compositionstart", (event) => {
  // `data` is `null` on a start that carries no text, which is every one of them here: the
  // textarea is emptied after each composition, so there is never anything to compose over.
  composition = event.data ?? "";
  draw();
});

imeInput.addEventListener("compositionupdate", (event) => {
  composition = event.data ?? "";
  draw();
});

imeInput.addEventListener("compositionend", (event) => {
  composition = "";
  draw();
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

for (const [name, button] of Object.entries(sampleButtons)) {
  button.addEventListener("click", () => {
    // A sample is a buffer rather than a file: nothing on the other side of `:w` was opened, so
    // whatever was open is let go of and the command reports that there is nowhere to write.
    closeOpenFile();
    void loadText(SAMPLES[name], name);
    focusEditor();
    report(`${name} を読み込みました`);
  });
}

if (window.showOpenFilePicker === undefined) {
  // Firefox and Safari have no File System Access API, and without it a page can read a file
  // through an `<input type="file">` but has nowhere to write it back to.
  localButton.disabled = true;
  localButton.title = "このブラウザは File System Access API に対応していません";
}

// The handle the E2E run drives and inspects the demo through.
window.wimDemo = {
  sendKeys: handleKeys,
  /**
   * Replaces the buffer, which is how the E2E run gets one taller than the viewport, under `name`
   * when it is to be highlighted as the language that name says. The answer settles once the
   * grammar has been fetched and the first highlighted frame is up.
   */
  load: loadText,
  /** Redraws every row, which the E2E run compares the damage-driven redraw against. */
  redraw: () => draw({ full: true }),
  /**
   * Settles once the transpiled plugins have been loaded and their commands registered, with the
   * commands there are. Nothing rejects: a plugin that could not be loaded is in `failures`.
   */
  plugins: () =>
    pluginsStarted.then(() => ({
      commands: [...pluginCommands.values()].map((command) => ({
        name: command.name,
        description: command.description,
        plugin: command.plugin,
      })),
      status: pluginStatus.textContent,
    })),
  /**
   * Settles once `wim.jsonc` has been read and its autocmds registered, with the ones it
   * declared. Nothing rejects: a demo served without a config binds nothing.
   */
  autocmds: () =>
    autocmdsStarted.then(() => ({
      declared: autocmds,
      list: autocmdList.textContent,
      status: autocmdStatus.textContent,
    })),
  /** The runs row `line` is coloured by, `null` for a buffer in no language the demo highlights. */
  highlightRuns: (line) => (highlighter === null ? null : highlighter.rowRuns(line)),
  state: () => ({
    text: editor.text(),
    lines: Array.from({ length: editor.line_count() }, (_, line) => editor.line(line)),
    cursor: { line: editor.cursor_line(), col: editor.cursor_col() },
    mode: editor.mode(),
    commandLine: editor.command_line() ?? null,
    damage: { start: lastOutcome.damageStart, end: lastOutcome.damageEnd },
    effects: lastOutcome.effects,
    viewport: { top: view.scrollTop, rows: view.visibleRows },
    highlight: {
      /** The grammar the buffer is parsed with, `null` when nothing highlights it. */
      language: highlighter?.language ?? null,
      /** The rows the last batch of keys changed the colours of, which were redrawn for it. */
      damage: [...highlightDamage].sort((left, right) => left - right),
    },
    plugin: {
      /** What running a plugin command last did, which is where its message or its error lands. */
      status: pluginStatus.textContent,
    },
    autocmd: {
      /** What the handlers the last batch of keys ran did, one entry each. */
      ran: lastAutocmds,
      status: autocmdStatus.textContent,
    },
    ime: {
      /** What an IME is composing, which the row is drawn with and the buffer does not hold. */
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
