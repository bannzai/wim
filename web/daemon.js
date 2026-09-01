// The client half of the daemon's protocol, spoken from JS.
//
// The daemon is a file system provider: it reads and writes the files under the directory it was
// started over, and the buffer being edited stays here in the browser
// (documents/adr/0001-daemon-fs-provider.md). What crosses the wire is JSON in WebSocket text
// frames, and the shape of that JSON is fixed by wim-protocol's own tests, so this module writes
// the objects out by hand rather than reaching them through another Wasm module.

/** The protocol version this client speaks, carried as `v` on every message. */
const PROTOCOL_VERSION = 1;

/**
 * `address` as the URL a WebSocket opens.
 *
 * What `wim serve` prints is a bare `127.0.0.1:PORT`, which is what a user has in front of them
 * to paste, so an address without a scheme is read as the plain WebSocket the daemon serves. A
 * full URL is left alone, which is how a daemon reached through a tunnel is named.
 */
function socketUrl(address) {
  const trimmed = address.trim();
  return /^wss?:\/\//i.test(trimmed) ? trimmed : `ws://${trimmed}`;
}

/** The socket at `url` once it is open, or an error when it never opens. */
function openSocket(url) {
  return new Promise((resolve, reject) => {
    let socket;
    try {
      socket = new WebSocket(url);
    } catch (error) {
      // A URL the constructor refuses — a port a page may not open, a scheme that is not ws —
      // is the one failure reported before the socket exists.
      reject(new Error(`${url} に接続できません: ${error.message}`));
      return;
    }
    socket.addEventListener("open", () => resolve(socket), { once: true });
    // A socket that fails to connect says only that it failed: why is kept from the page, so
    // that it cannot use a WebSocket to work out what is listening on the machine it runs on.
    socket.addEventListener("error", () => reject(new Error(`${url} に接続できません`)), {
      once: true,
    });
  });
}

/**
 * Connects to the daemon at `address` and presents `token`.
 *
 * What comes back reads and writes files over that one connection, and works until it is closed
 * or the daemon goes away. The token goes out before the connection is handed over because it
 * has to be the first message: a daemon answers anything else with an error and hangs up.
 */
export async function connect(address, token) {
  const socket = await openSocket(socketUrl(address));
  /** The requests waiting for the response that names their id, keyed by that id. */
  const pending = new Map();
  // From 1: the daemon answers a message it could not read an id out of under id 0, which the
  // protocol keeps for exactly that, so no request of this client's is ever waiting on it.
  let nextId = 1;

  /** Fails every request in flight, which is what a connection that ended leaves behind. */
  function fail(error) {
    for (const waiting of pending.values()) {
      waiting.reject(error);
    }
    pending.clear();
  }

  socket.addEventListener("message", (event) => {
    let message;
    try {
      message = JSON.parse(event.data);
    } catch {
      // Every frame the daemon sends is one wim-protocol serialized, so a frame that is not
      // JSON leaves this client unable to say which request it answered, and the connection is
      // no longer one to send the next request over.
      socket.close();
      fail(new Error("デーモンの応答を読み取れません"));
      return;
    }
    if (message.event !== undefined) {
      // A push rather than an answer. Nothing here starts a watch, so there is none to report.
      return;
    }
    const waiting = pending.get(message.id);
    if (waiting === undefined) {
      return;
    }
    pending.delete(message.id);
    if (message.error === undefined) {
      waiting.resolve(message.result);
    } else {
      waiting.reject(new Error(message.error.message));
    }
  });
  socket.addEventListener("close", () => fail(new Error("デーモンとの接続が切れました")));

  /** Sends one request and answers with its result, or throws what the daemon answered with. */
  function request(method, params) {
    if (socket.readyState !== WebSocket.OPEN) {
      return Promise.reject(new Error("デーモンとの接続が切れています"));
    }
    const id = nextId;
    nextId += 1;
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject });
      socket.send(JSON.stringify({ v: PROTOCOL_VERSION, id, method, params }));
    });
  }

  try {
    await request("auth", { token });
  } catch (error) {
    // The daemon drops a connection that opened with a token it did not recognise; closing here
    // is for the connection it answered without dropping, such as one of another version.
    socket.close();
    throw error;
  }

  return {
    /** The whole of the file at `path`, read under the directory the daemon serves. */
    async read(path) {
      return (await request("fs.read", { path })).content;
    },
    /** Replaces the file at `path` with `content`, creating it when it is not there. */
    async write(path, content) {
      await request("fs.write", { path, content });
    },
    close() {
      socket.close();
    },
  };
}
