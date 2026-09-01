// The browser half of the plugin host: it loads the modules `scripts/transpile-plugins.sh`
// generated from the same `.wasm` components the native host loads, and holds them to the same
// ABI before a single command is registered.
//
// What the two hosts check differs only in where the answer comes from. Natively wasmtime is
// handed the component and the host reads the version out of its export names
// (`crates/wim-plugin-host/src/lib.rs`); here jco has already turned those names into the names
// of an ES module's exports, so the same version is read off `wim:plugin/commands@0.1.0` in
// `Object.keys`. A module built against another ABI exports another name and is refused, the way
// wasmtime refuses to find the export the native bindings look up.
//
// The sandbox needs nothing at run time. The world's one import carries types and no functions,
// so jco writes a module that imports nothing — no WASI shim, no host functions — and that is
// checked where it is decided, in the transpile step.

/**
 * What the transpile step wrote: the ABI the build speaks, and the plugins it generated. Its
 * absence is what a checkout that has not run `make build-web-plugins` looks like, which is every
 * one that cannot build a component.
 */
const MANIFEST = "./plugins/manifest.json";

/**
 * The interface the commands live on, whose versioned export name carries the ABI. The other two
 * of the world (`events`, `ui`) are exported alongside it and the demo does not call them yet.
 */
const COMMANDS = "wim:plugin/commands";

/**
 * Loads every plugin the build transpiled, as commands keyed by the name `:name` runs them under.
 *
 * Nothing here throws: a demo served without plugins is the normal state of a checkout that
 * cannot build components, and a plugin that fails to load is reported rather than taking the
 * rest of them down with it.
 */
export async function loadPlugins() {
  let manifest;
  try {
    const response = await fetch(MANIFEST);
    if (!response.ok) {
      throw new Error(`${response.status}`);
    }
    manifest = await response.json();
  } catch {
    return { abi: null, commands: new Map(), failures: [] };
  }

  const commands = new Map();
  const failures = [];
  for (const plugin of manifest.plugins) {
    try {
      for (const command of await loadPlugin(plugin, manifest.abi)) {
        commands.set(command.name, command);
      }
    } catch (error) {
      failures.push(`${plugin.name}: ${error.message}`);
    }
  }
  return { abi: manifest.abi, commands, failures };
}

/** The commands one plugin publishes, once it has been held to `abi`. */
async function loadPlugin(plugin, abi) {
  const module = await import(plugin.module);
  const commands = module[`${COMMANDS}@${abi}`];
  if (commands === undefined) {
    throw new Error(abiComplaint(module, abi));
  }
  return commands.listCommands().map((command) => ({
    name: command.name,
    description: command.description,
    plugin: plugin.name,
    /**
     * Runs the command over `buffer`, answering with the edit the host is to apply. What the
     * plugin refuses comes back as a `ComponentError` whose message is its own wording, which
     * is the `Err(String)` half of the ABI's `result<edit, string>`.
     */
    run: (args, buffer) => commands.run(command.name, args, buffer),
  }));
}

/**
 * Why a module the manifest named is not a plugin this host can load, in the two shapes the
 * native host reports: an ABI other than this one, or nothing of `wim:plugin` at all.
 */
function abiComplaint(module, abi) {
  const found = Object.keys(module).find((name) => name.startsWith(`${COMMANDS}@`));
  if (found === undefined) {
    return `no ${COMMANDS} interface is exported`;
  }
  return `built against ${found.slice(COMMANDS.length + 1)}, and this host speaks ${abi}`;
}
