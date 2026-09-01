// Static file server for this directory, used by `make web` and by the Playwright run.
//
// It is in the repo rather than pulled from npm so that a checkout can serve the demo with
// nothing installed, and so that the wasm goes out as `application/wasm`, which is what lets
// the browser compile it while it streams.

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(fileURLToPath(new URL(".", import.meta.url)));
// 4173 is the port the Playwright config expects; `node serve.mjs <port>` overrides it.
const PORT = Number(process.argv[2] ?? 4173);

const CONTENT_TYPES = {
  ".css": "text/css; charset=utf-8",
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  // JSONC is JSON with comments, which is not JSON: it is served as text so that nothing on the
  // way tries to read it as the JSON it is not (`documents/CONFIG.md`).
  ".jsonc": "text/plain; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".wasm": "application/wasm",
};

const server = createServer(async (request, response) => {
  const path = new URL(request.url, `http://127.0.0.1:${PORT}`).pathname;
  const file = join(ROOT, normalize(path === "/" ? "/index.html" : decodeURIComponent(path)));
  if (!file.startsWith(ROOT)) {
    response.writeHead(403).end();
    return;
  }
  try {
    const body = await readFile(file);
    response.writeHead(200, {
      "content-type": CONTENT_TYPES[extname(file)] ?? "application/octet-stream",
    });
    response.end(body);
  } catch {
    response.writeHead(404).end();
  }
});

server.listen(PORT, "127.0.0.1", () => {
  console.log(`serving ${ROOT} on http://127.0.0.1:${PORT}/`);
});
