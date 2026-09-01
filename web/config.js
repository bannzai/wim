// The browser half of reading `wim.jsonc`: the same file the native host reads, in the same
// dialect and against the same schema (`documents/CONFIG.md`).
//
// What the two hosts do differs only in what reads the dialect. Natively that is the
// `jsonc-parser` crate, held to comments and trailing commas and nothing else
// (`crates/wim/src/config.rs`); here it is `stripJsonc` below, which takes those two away and
// leaves `JSON.parse` with plain JSON. A config either host accepts and the other does not would
// be worse than one neither does, which is why the dialect is spelled out on both sides rather
// than left to what a parser happens to allow.

/**
 * The events an autocmd can be bound to, which is `wim_core::Event::names` written out for the
 * host that cannot call it. A config naming anything else is refused here, the way the native
 * host refuses it, rather than leaving a handler that would never run to be found by hand.
 */
const EVENTS = ["buffer-write", "buffer-write-post", "text-changed", "mode-changed"];

/** The kinds of handler a config may declare, and the field each one carries. */
const HANDLERS = { ex: "command", keys: "keys", plugin: "plugin" };

/**
 * Reads the config at `url`, answering with its autocmds.
 *
 * A demo served without a config is the normal state of one: nothing is bound, and the answer is
 * a config that binds nothing. A config that is there but cannot be read is another matter and
 * comes back as `error`, since something was meant to be bound and is not.
 */
export async function loadConfig(url) {
  let text;
  try {
    const response = await fetch(url);
    if (!response.ok) {
      throw new Error(`${response.status}`);
    }
    text = await response.text();
  } catch {
    return { autocmds: [], error: null };
  }
  try {
    return { autocmds: parseConfig(text), error: null };
  } catch (error) {
    return { autocmds: [], error: error.message };
  }
}

/**
 * The autocmds a config declares, in the order they are written.
 *
 * Throws when the text is not JSONC, or is JSONC that is not a config: an unknown field, a
 * handler of a kind that does not exist, or an event nothing raises.
 */
export function parseConfig(text) {
  // An empty file is a config that binds nothing rather than a config that is missing.
  const config = JSON.parse(stripJsonc(text) || "{}");
  if (config === null || typeof config !== "object" || Array.isArray(config)) {
    throw new Error("設定はオブジェクトで書いてください");
  }
  return checkedAutocmds(config);
}

/** The `autocmds` of a config, once every one of them has been checked. */
function checkedAutocmds(config) {
  for (const field of Object.keys(config)) {
    if (field !== "autocmds") {
      throw new Error(`${field} は設定にない項目です`);
    }
  }
  const autocmds = config.autocmds ?? [];
  if (!Array.isArray(autocmds)) {
    throw new Error("autocmds は配列で書いてください");
  }
  return autocmds.map((autocmd) => checkedAutocmd(autocmd));
}

/** One autocmd, checked against the schema. */
function checkedAutocmd(autocmd) {
  for (const field of Object.keys(autocmd)) {
    if (field !== "event" && field !== "handler") {
      throw new Error(`${field} は autocmd にない項目です`);
    }
  }
  const { event, handler } = autocmd;
  if (!EVENTS.includes(event)) {
    throw new Error(`${event} というイベントはありません (${EVENTS.join(", ")})`);
  }
  const field = HANDLERS[handler?.kind];
  if (field === undefined) {
    throw new Error(`${handler?.kind} という handler の kind はありません`);
  }
  for (const written of Object.keys(handler)) {
    if (written !== "kind" && written !== field) {
      throw new Error(`${written} は kind が ${handler.kind} の handler にない項目です`);
    }
  }
  if (typeof handler[field] !== "string") {
    throw new Error(`kind が ${handler.kind} の handler には ${field} が要ります`);
  }
  return { event, handler };
}

/**
 * `text` with its comments and trailing commas taken out, which is what turns JSONC into the JSON
 * `JSON.parse` reads.
 *
 * The two go in that order so that the second only ever looks at what the first left: a trailing
 * comma with a comment between it and the bracket it trails is a comma the second pass sees next
 * to that bracket.
 */
export function stripJsonc(text) {
  return dropTrailingCommas(dropComments(text)).trim();
}

/**
 * `text` without its line comments and its block comments.
 *
 * The walk is character by character because a comment is only a comment outside a string: a
 * `"https://wim"` in a config is a value rather than the start of one. Escapes inside a string
 * are stepped over for the same reason — a `"\\"` ends there, and reading the backslash as an
 * escape would run the string on into the rest of the file.
 */
function dropComments(text) {
  let stripped = "";
  let index = 0;
  while (index < text.length) {
    const character = text[index];
    if (character === '"') {
      const end = endOfString(text, index);
      stripped += text.slice(index, end);
      index = end;
      continue;
    }
    if (character === "/" && text[index + 1] === "/") {
      // The line break itself is kept: it is what a `//` comment on the end of a line hid.
      const end = text.indexOf("\n", index);
      index = end === -1 ? text.length : end;
      continue;
    }
    if (character === "/" && text[index + 1] === "*") {
      const end = text.indexOf("*/", index + 2);
      index = end === -1 ? text.length : end + 2;
      // A block comment stood between two values, which is a place a space may stand.
      stripped += " ";
      continue;
    }
    stripped += character;
    index += 1;
  }
  return stripped;
}

/** `text` without the commas that trail the last value of an object or an array. */
function dropTrailingCommas(text) {
  let stripped = "";
  let index = 0;
  while (index < text.length) {
    if (text[index] === '"') {
      const end = endOfString(text, index);
      stripped += text.slice(index, end);
      index = end;
      continue;
    }
    // A comma is trailing when nothing but blanks stands between it and the bracket it is
    // inside; a comma inside a string never reaches here.
    if (text[index] === "," && /^\s*[}\]]/.test(text.slice(index + 1))) {
      index += 1;
      continue;
    }
    stripped += text[index];
    index += 1;
  }
  return stripped;
}

/** One past the closing quote of the string that starts at `start`. */
function endOfString(text, start) {
  let index = start + 1;
  while (index < text.length) {
    if (text[index] === "\\") {
      index += 2;
      continue;
    }
    if (text[index] === '"') {
      return index + 1;
    }
    index += 1;
  }
  // An unterminated string is JSON's to complain about, in wording about the string rather than
  // about the stripping.
  return text.length;
}
