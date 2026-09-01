// The browser half of the plugin host: it loads the modules `scripts/transpile-plugins.sh`
// generated from the same `.wasm` components the native host loads, and holds them to the same
// ABI before a single command is registered.
//
// What the two hosts check differs only in where the answer comes from. Natively wasmtime is
// handed the component and the host reads the version out of its export names
// (`crates/wim-plugin-host/src/lib.rs`); here jco has already turned those names into the names
// of an ES module's exports, so the same version is read off `wim:plugin/commands@0.1.0` in
// `Object.keys`. A module built against another ABI exports another name and is refused, the way
// wasmtime refuses to find the export the native bindings look up. A module that carries some of
// the world's interfaces and not the rest is refused there too, so all three are asked for here
// while the plugin is loaded rather than when one of them is first called.
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
 * of the world (`events`, `ui`) are exported alongside it, and `events` is what an autocmd of
 * kind `plugin` reaches (`documents/CONFIG.md`). All three are looked up while the plugin is
 * loaded, so the ABI a module is held to is the whole of the world and not the commands alone.
 */
const COMMANDS = "wim:plugin/commands";

/** The interface an event is delivered over. */
const EVENTS = "wim:plugin/events";

/** The interface a panel is rendered over. */
const UI = "wim:plugin/ui";

/**
 * Loads every plugin the build transpiled: the commands keyed by the name `:name` runs them
 * under, and the plugins themselves keyed by the name an autocmd names them by.
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
    return { abi: null, commands: new Map(), plugins: new Map(), failures: [] };
  }

  const commands = new Map();
  const plugins = new Map();
  const failures = [];
  for (const declared of manifest.plugins) {
    try {
      const { published, plugin } = await loadPlugin(declared, manifest.abi);
      for (const command of published) {
        commands.set(command.name, command);
      }
      plugins.set(plugin.name, plugin);
    } catch (error) {
      failures.push(`${declared.name}: ${error.message}`);
    }
  }
  return { abi: manifest.abi, commands, plugins, failures };
}

/** What one plugin publishes, once it has been held to `abi`. */
async function loadPlugin(declared, abi) {
  const module = await import(declared.module);
  const commands = exportedInterface(module, COMMANDS, abi);
  const events = exportedInterface(module, EVENTS, abi);
  const ui = exportedInterface(module, UI, abi);
  const published = commands.listCommands().map((command) => ({
    name: command.name,
    description: command.description,
    plugin: declared.name,
    /**
     * Runs the command over `buffer`, answering with the edit the host is to apply. What the
     * plugin refuses comes back as a `ComponentError` whose message is its own wording, which
     * is the `Err(String)` half of the ABI's `result<edit, string>`.
     */
    run: (args, buffer) => commands.run(command.name, args, buffer),
  }));
  return {
    published,
    plugin: {
      name: declared.name,
      /** The event names it asked for. The host delivers nothing else (`wit/plugin.wit`). */
      subscriptions: events.subscriptions(),
      /** Delivers one of those events, answering with an edit the way a command does. */
      onEvent: (event, buffer) => events.onEvent(event, buffer),
      /**
       * The panel to show over `buffer`, `undefined` when the plugin has none — which is the
       * `none` of the ABI's `option<panel>`, and the host's cue to close the panel
       * (`wit/plugin.wit`). What comes back is HTML this host does not trust; where it may be
       * drawn is decided in `web/main.js`.
       */
      render: (buffer) => ui.render(buffer),
    },
  };
}

/**
 * The `interface` a module exports at this ABI, refusing the module that exports no such thing.
 *
 * Every interface of the world is asked for while the plugin is loaded rather than where it is
 * first called, because a module that is missing one is not a plugin either host can run: natively
 * the bindings look up all three exports as the component is instantiated, so an incomplete
 * component never loads at all (`crates/wim-plugin-host/src/lib.rs`). Deferring the lookup would
 * leave the commands of such a plugin registered and its panel failing on every refresh.
 */
function exportedInterface(module, interfaceName, abi) {
  const exported = module[`${interfaceName}@${abi}`];
  if (exported === undefined) {
    throw new Error(abiComplaint(module, interfaceName, abi));
  }
  return exported;
}

/**
 * Why a module the manifest named is not a plugin this host can load, in the two shapes the
 * native host reports: an ABI other than this one, or nothing of that interface at all.
 */
function abiComplaint(module, interfaceName, abi) {
  const found = Object.keys(module).find((name) => name.startsWith(`${interfaceName}@`));
  if (found === undefined) {
    return `no ${interfaceName} interface is exported`;
  }
  return `built against ${found.slice(interfaceName.length + 1)}, and this host speaks ${abi}`;
}
