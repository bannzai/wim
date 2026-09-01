// Syntax highlighting for the demo, which tree-sitter does the parsing for.
//
// wim-core stays a pure crate that knows nothing about languages (documents/PROJECT.md), so all
// of this sits on the browser side of the wasm boundary: the buffer's text goes into tree-sitter,
// and what comes back is, for each row, the runs of columns that are drawn in a colour of their
// own rather than in the plain text colour.
//
// The runtime and the grammars are the files `scripts/vendor-tree-sitter.sh` puts under
// `vendor/tree-sitter/`. Adding a language is a row in `LANGUAGES` here and a row in `GRAMMARS`
// there; nothing else in the demo names a language.

import { Language, Parser, Query } from "./vendor/tree-sitter/web-tree-sitter.js";

/** Where the vendored grammars are, resolved against this module so the demo can be served from any path. */
const VENDOR = new URL("./vendor/tree-sitter/", import.meta.url);

/**
 * The languages the demo highlights, keyed by the file extension that names one. The value is
 * the name `vendor/tree-sitter/<name>.wasm` and `<name>.highlights.scm` are installed under.
 */
const LANGUAGES = new Map([
  ["rs", "rust"],
  ["md", "markdown"],
]);

/**
 * The colours a highlighted run is drawn in, over `COLORS.text` in `main.js`, which everything
 * without a colour of its own keeps.
 *
 * Six roles is what it takes to tell the two grammars' captures apart while staying legible on
 * the demo's dark background: any more and the rows read as a colour chart rather than as code.
 * The comment grey is dimmer than the gutter's `#6a7383` so that a comment reads as text that has
 * been played down rather than as chrome.
 */
const HIGHLIGHT_COLORS = {
  keyword: "#c792ea",
  string: "#c3e88d",
  comment: "#5c6370",
  function: "#82aaff",
  type: "#ffcb6b",
  constant: "#f78c6c",
};

/**
 * The role each capture is drawn in.
 *
 * tree-sitter names captures in a dotted hierarchy, and a name that is not in here is looked up
 * again with its last dotted part dropped — `function.macro` finds `function` — so only the names
 * where the two grammars disagree need a row of their own. A capture no row is reached from is
 * drawn plain, which is what happens to Rust's brackets, operators and field names.
 *
 * `null` is a capture that is deliberately plain rather than unmapped: the markdown grammar puts
 * `@none` on the contents of a fenced code block, inside the `@text.literal` covering the block.
 */
const CAPTURE_ROLES = new Map([
  ["keyword", "keyword"],
  ["attribute", "keyword"],
  ["label", "keyword"],
  ["variable.builtin", "keyword"],
  // The markdown grammar's `#` heading markers, list markers and thematic breaks.
  ["punctuation.special", "keyword"],
  ["text.title", "keyword"],
  ["string", "string"],
  ["escape", "string"],
  // Markdown's code spans and fenced blocks, whose contents `@none` then takes back to plain.
  ["text.literal", "string"],
  ["comment", "comment"],
  ["function", "function"],
  ["constructor", "function"],
  ["text.uri", "function"],
  ["type", "type"],
  ["text.reference", "type"],
  // Rust captures its number and boolean literals as `constant.builtin` rather than as numbers.
  ["constant", "constant"],
  ["none", null],
]);

/** The name of the language `path` is written in, or `null` for a name no grammar is loaded for. */
export function languageOf(path) {
  const extension = path.slice(path.lastIndexOf(".") + 1).toLowerCase();
  return LANGUAGES.get(extension) ?? null;
}

/** The colour `capture` is drawn in, or `null` where it is drawn as plain text. */
function colorOf(capture) {
  for (let name = capture; name !== ""; name = name.slice(0, Math.max(0, name.lastIndexOf(".")))) {
    const role = CAPTURE_ROLES.get(name);
    if (role !== undefined) {
      return role === null ? null : HIGHLIGHT_COLORS[role];
    }
    if (!name.includes(".")) {
      return null;
    }
  }
  return null;
}

/** The runtime, which is loaded once however many languages are asked for. */
let started;

/** The languages already loaded, so that reopening a file does not fetch the grammar again. */
const loaded = new Map();

/** The grammar and the query for `name`, fetched from `vendor/tree-sitter/` on first use. */
async function grammarOf(name) {
  const cached = loaded.get(name);
  if (cached !== undefined) {
    return cached;
  }
  started ??= Parser.init();
  await started;
  const language = await Language.load(new URL(`${name}.wasm`, VENDOR).href);
  const source = await fetch(new URL(`${name}.highlights.scm`, VENDOR)).then((response) => {
    if (!response.ok) {
      throw new Error(`${name}.highlights.scm: ${response.status}`);
    }
    return response.text();
  });
  const grammar = { language, query: new Query(language, source) };
  loaded.set(name, grammar);
  return grammar;
}

/** Whether `index` sits between the two halves of a surrogate pair, which is not a character boundary. */
function splitsSurrogate(text, index) {
  const unit = text.charCodeAt(index);
  return unit >= 0xdc00 && unit <= 0xdfff;
}

/** Where `index` is in `text`, as the row and the column tree-sitter counts in UTF-16 units. */
function pointAt(text, index) {
  let row = 0;
  let lineStart = 0;
  for (let at = text.indexOf("\n"); at !== -1 && at < index; at = text.indexOf("\n", at + 1)) {
    row += 1;
    lineStart = at + 1;
  }
  return { row, column: index - lineStart };
}

/**
 * The one contiguous edit that turns `before` into `after`, or `null` when nothing changed.
 *
 * tree-sitter reparses from an edit rather than from a diff, and the texts' common prefix and
 * suffix pin exactly one down. The damage wim-core reports cannot stand in for it: a damaged line
 * range says which rows have to be redrawn, not where in the text the change was, and `dd` damages
 * every row down to the end of the buffer for one line coming out.
 */
function editBetween(before, after) {
  if (before === after) {
    return null;
  }
  const shortest = Math.min(before.length, after.length);
  let prefix = 0;
  while (prefix < shortest && before.charCodeAt(prefix) === after.charCodeAt(prefix)) {
    prefix += 1;
  }
  // A prefix that ends between the halves of a surrogate pair would put the edit in the middle of
  // a character, which is not a position tree-sitter can be edited at.
  if (prefix > 0 && prefix < shortest && splitsSurrogate(after, prefix)) {
    prefix -= 1;
  }
  let suffix = 0;
  while (
    suffix < shortest - prefix &&
    before.charCodeAt(before.length - 1 - suffix) === after.charCodeAt(after.length - 1 - suffix)
  ) {
    suffix += 1;
  }
  if (suffix > 0 && splitsSurrogate(after, after.length - suffix)) {
    suffix -= 1;
  }
  return {
    startIndex: prefix,
    oldEndIndex: before.length - suffix,
    newEndIndex: after.length - suffix,
    startPosition: pointAt(after, prefix),
    oldEndPosition: pointAt(before, before.length - suffix),
    newEndPosition: pointAt(after, after.length - suffix),
  };
}

/**
 * `runs` with `span` painted over it, dropping whatever the runs already there said about the
 * columns it covers.
 *
 * Captures nest — a Rust attribute covers the type named inside it, a markdown fence covers the
 * code in it — and the innermost one is the one to draw, so spans are painted longest first and
 * the shorter ones land on top.
 */
function paintOver(runs, span) {
  const painted = [];
  for (const run of runs) {
    if (run.end <= span.start || run.start >= span.end) {
      painted.push(run);
      continue;
    }
    if (run.start < span.start) {
      painted.push({ start: run.start, end: span.start, color: run.color });
    }
    if (run.end > span.end) {
      painted.push({ start: span.end, end: run.end, color: run.color });
    }
  }
  painted.push(span);
  return painted.sort((left, right) => left.start - right.start);
}

/**
 * Whether two rows' runs would draw the same, which is what says a row needs redrawing.
 *
 * A row nothing was ever asked about counts as having had no runs: the rows under the end of the
 * buffer are drawn as the end-of-buffer filler and never ask, and calling every one of them
 * changed would put a row of `~` in the damage on every key.
 */
function sameRuns(left, right) {
  const previous = left ?? [];
  return (
    previous.length === right.length &&
    previous.every(
      (run, index) =>
        run.start === right[index].start &&
        run.end === right[index].end &&
        run.color === right[index].color,
    )
  );
}

/**
 * A highlighter over one buffer in one language: it holds the parse tree, moves it along with the
 * edits, and answers which columns of a row are drawn in which colour.
 *
 * Rows are answered one at a time and cached, because that is what the demo draws: a key redraws
 * the damaged rows and a frame never draws more than a viewport of them, so the query never runs
 * over more of the buffer than is on screen.
 */
export async function createHighlighter(name, text) {
  const { language, query } = await grammarOf(name);
  const parser = new Parser();
  parser.setLanguage(language);

  let source = text;
  let tree = parser.parse(source);
  /** Row to the runs it draws, emptied on every reparse so that a shifted row is never stale. */
  let runs = new Map();

  /** The runs row `row` is drawn with, `end` running past the row for a capture that carries on. */
  function runsOfRow(row) {
    const spans = query
      .captures(tree.rootNode, {
        startPosition: { row, column: 0 },
        endPosition: { row: row + 1, column: 0 },
      })
      .map(({ name: capture, node }) => ({
        start: node.startPosition.row < row ? 0 : node.startPosition.column,
        // A capture that ends on a later row covers the rest of this one, however long it is.
        end: node.endPosition.row > row ? Number.POSITIVE_INFINITY : node.endPosition.column,
        color: colorOf(capture),
      }))
      .filter((span) => span.end > span.start)
      // Longest first, so that `paintOver` lands the innermost capture on top. Subtracting the
      // lengths would compare two spans that both run past the row as `Infinity - Infinity`.
      .sort((left, right) => {
        const shorter = left.end - left.start < right.end - right.start;
        return shorter ? 1 : left.end - left.start === right.end - right.start ? 0 : -1;
      });
    return spans.reduce(paintOver, []).filter((span) => span.color !== null);
  }

  /** The runs row `row` is drawn with, computed on first ask and kept until the next reparse. */
  function rowRuns(row) {
    const cached = runs.get(row);
    if (cached !== undefined) {
      return cached;
    }
    const computed = runsOfRow(row);
    runs.set(row, computed);
    return computed;
  }

  /**
   * Moves the tree on to `next` and answers which of `rows` are drawn differently because of it.
   *
   * Those rows go in with the damage wim-core reported, because highlighting changes rows the edit
   * never touched: a quote typed on one line puts every line under it inside a string.
   */
  function update(next, rows) {
    const edit = editBetween(source, next);
    if (edit === null) {
      return new Set();
    }
    const previous = runs;
    tree.edit(edit);
    const reparsed = parser.parse(next, tree);
    if (reparsed === null) {
      // Nothing here asks the parser to give up part way, so this is unreachable; the tree that
      // is already there is kept rather than swapped for nothing.
      return new Set();
    }
    tree.delete();
    tree = reparsed;
    source = next;
    runs = new Map();
    const changed = new Set();
    for (const row of rows) {
      if (!sameRuns(previous.get(row), rowRuns(row))) {
        changed.add(row);
      }
    }
    return changed;
  }

  /** Lets go of the tree, which lives in the runtime's memory rather than the collector's. */
  function close() {
    tree.delete();
  }

  return { language: name, rowRuns, update, close };
}
