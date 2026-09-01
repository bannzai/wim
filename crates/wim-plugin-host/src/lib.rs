//! The native host for wim plugins: it loads a `.wasm` component that implements the
//! `wim:plugin/plugin` world of `wit/plugin.wit` and calls into it through wasmtime.
//!
//! [`Plugin::from_file`] compiles a component, checks that it was built against an ABI this host
//! understands, and instantiates it. The calls on [`Plugin`] are the four entry points of the
//! world: [`Plugin::list_commands`] and [`Plugin::run`] for the `commands` interface,
//! [`Plugin::subscriptions`] and [`Plugin::on_event`] for `events`, and [`Plugin::render`] for
//! `ui`.
//!
//! # The sandbox
//!
//! The world imports one interface, `wim:plugin/buffer`, and that interface only carries types:
//! there is not a single host function a plugin can call. Nothing is added to the linker here —
//! no WASI, no clock, no environment — so a plugin has no way to open a file, reach the network
//! or read the time, and a component that asks for any of it is refused when it is loaded rather
//! than trapped once it runs. Everything a plugin is allowed to see arrives as a [`Snapshot`]
//! passed by value, and everything it can change goes back as an [`Edit`] the host applies
//! itself (`wit/README.md`).
//!
//! What the linker withholds is capability; what a plugin can still spend is time and memory,
//! and every call runs on the caller's thread. So the store meters both: each call is given
//! [`CALL_FUEL`] to burn and the guest's memory may not grow past [`MEMORY_LIMIT`]. A plugin
//! that loops forever or keeps allocating trips one of the two and comes back as an
//! [`Error::Wasm`] rather than taking the editor down with it.
//!
//! ```no_run
//! # fn load() -> Result<(), wim_plugin_host::Error> {
//! use wim_plugin_host::{Edit, Plugin, Position, Snapshot};
//!
//! let mut plugin = Plugin::from_file("hello_wim.wasm")?;
//! let buffer = Snapshot {
//!     name: "notes.txt".to_string(),
//!     text: "hello\n".to_string(),
//!     cursor: Position { line: 0, column: 0 },
//! };
//! assert_eq!(plugin.run("upcase", &[], &buffer)?, Edit::ReplaceAll("HELLO\n".to_string()));
//! # Ok(())
//! # }
//! ```

mod bindings {
    // Generated from the same `wit/plugin.wit` the plugins build against, so the two halves of
    // the ABI cannot be generated from different sources. Nothing here is written by hand; the
    // module exists to keep the generated names out of the crate root.
    wasmtime::component::bindgen!({
        path: "../../wit",
        world: "plugin",
        additional_derives: [PartialEq],
    });
}

use std::error;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};

pub use bindings::exports::wim::plugin::commands::Command;
pub use bindings::exports::wim::plugin::events::Event;
pub use bindings::exports::wim::plugin::ui::Panel;
pub use bindings::wim::plugin::buffer::{Edit, LineEdit, Position, Snapshot};

/// The wit the bindings above are generated from, read in again so that the ABI version this
/// host accepts comes from the same file rather than from a constant that could drift away from
/// it.
const PLUGIN_WIT: &str = include_str!("../../../wit/plugin.wit");

/// The wit package the world lives in. Export names carry it, which is how a loaded component
/// tells the host which ABI it was built against.
const PACKAGE: &str = "wim:plugin";

/// The 8 bytes a wasm component opens with: the `\0asm` magic followed by layer 1. A core module
/// carries version 1 in the same place, which is what tells the two apart — the same check
/// `scripts/check-wasm-component.sh` makes on the build output.
const COMPONENT_HEADER: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00];

/// The fuel one guest call may burn before wasmtime stops it, given again for every call so that
/// a plugin the editor keeps loaded is bounded per call rather than over its life.
///
/// A unit of fuel is about one wasm instruction, so what this bounds is work; what that costs in
/// time was measured. The number sits between the two:
///
/// - what a call spends grows with the buffer, since every interface of the world is a transform
///   over one [`Snapshot`]. Upcasing a byte or writing it into a panel is a handful of
///   instructions, and the canonical ABI copies the buffer in and the answer back out; at twenty
///   instructions a byte, generous for both, a 1 MiB buffer comes to 2e7 — and the files wim
///   edits are far smaller than that.
/// - burning the budget to the end, which is what a plugin that never returns does, took ~0.3 s
///   on an Apple M4 Max. That is how long the editor pauses before it is told the plugin is gone.
const CALL_FUEL: u64 = 100_000_000;

/// The bytes the guest's linear memory may grow to.
///
/// A plugin is given the buffer by value and answers by value, so its memory has to hold the
/// snapshot, the answer, and whatever its allocator keeps between the two — a few times the
/// buffer, plus the module's own statics. 64 MiB is room to work over a buffer of several MiB,
/// well past the source files wim edits, while a component that grows memory in a loop stops
/// here rather than at the host's address space.
const MEMORY_LIMIT: usize = 64 * 1024 * 1024;

/// What can go wrong between reading a `.wasm` off disk and getting an answer out of it.
#[derive(Debug)]
pub enum Error {
    /// The file could not be read.
    Io(io::Error),
    /// The bytes are not a wasm component: either not wasm at all, or a core module, which is
    /// what a plugin built for the wrong target comes out as.
    NotAComponent,
    /// The component is one, but exports nothing from the `wim:plugin` package.
    NotAPlugin,
    /// The component was built against an ABI version that is not exactly this host's.
    AbiMismatch {
        /// The version this host was built against.
        expected: String,
        /// The version the component's exports are named with.
        found: String,
    },
    /// wasmtime refused the component or the guest trapped. Imports the host does not provide —
    /// WASI above all — end up here, and so does a plugin stopped for spending more than
    /// [`CALL_FUEL`] or growing past [`MEMORY_LIMIT`].
    Wasm(wasmtime::Error),
    /// The plugin ran and reported a failure of its own, in the wording it wants shown.
    Plugin(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(error) => write!(f, "{error}"),
            Error::NotAComponent => write!(
                f,
                "not a wasm component: a plugin is a core module turned into a component by \
                 `make build-plugins`, and this file was not"
            ),
            Error::NotAPlugin => write!(f, "no `{PACKAGE}` interface is exported"),
            Error::AbiMismatch { expected, found } => write!(
                f,
                "built against {PACKAGE}@{found}, and this host speaks {PACKAGE}@{expected}"
            ),
            Error::Wasm(error) => write!(f, "{error:#}"),
            Error::Plugin(message) => write!(f, "{message}"),
        }
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Error::Io(error) => Some(error),
            Error::Wasm(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::Io(error)
    }
}

impl From<wasmtime::Error> for Error {
    fn from(error: wasmtime::Error) -> Self {
        Error::Wasm(error)
    }
}

/// A loaded plugin: the component, the store it runs in, and the exports the world promises.
///
/// The calls take `&mut self` because each of them runs guest code, which the store owns. A
/// plugin holds no state between calls that the host can see: what it is given is a [`Snapshot`]
/// and what it gives back is an [`Edit`].
pub struct Plugin {
    store: Store<StoreLimits>,
    bindings: bindings::Plugin,
}

impl Plugin {
    /// Loads the component at `path`.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::from_binary(&fs::read(path)?)
    }

    /// Loads a component already in memory.
    pub fn from_binary(wasm: &[u8]) -> Result<Self, Error> {
        if !wasm.starts_with(&COMPONENT_HEADER) {
            return Err(Error::NotAComponent);
        }
        let engine = Engine::new(&engine_config())?;
        let component = Component::from_binary(&engine, wasm)?;
        check_abi(&engine, &component)?;
        // The linker is left empty on purpose: an empty linker is the sandbox. The world's one
        // import, `wim:plugin/buffer`, carries types and no functions, and wasmtime satisfies
        // such an instance without anything being defined for it, so a component that imports
        // more than that — any WASI interface — fails to instantiate here.
        let linker = Linker::<StoreLimits>::new(&engine);
        let mut store = Store::new(&engine, store_limits());
        store.limiter(|limits| limits);
        // Instantiating runs guest code too — the module's own initialisers — so the budget is
        // in place before the component is built rather than before the first call.
        store.set_fuel(CALL_FUEL)?;
        let bindings = bindings::Plugin::instantiate(&mut store, &component, &linker)?;
        Ok(Plugin { store, bindings })
    }

    /// The commands the plugin publishes, which the host registers as Ex commands.
    pub fn list_commands(&mut self) -> Result<Vec<Command>, Error> {
        self.refuel()?;
        Ok(self
            .bindings
            .wim_plugin_commands()
            .call_list_commands(&mut self.store)?)
    }

    /// Runs one of those commands over `buffer`.
    pub fn run(&mut self, name: &str, args: &[String], buffer: &Snapshot) -> Result<Edit, Error> {
        self.refuel()?;
        self.bindings
            .wim_plugin_commands()
            .call_run(&mut self.store, name, args, buffer)?
            .map_err(Error::Plugin)
    }

    /// The event names the plugin wants delivered. The host delivers nothing else.
    pub fn subscriptions(&mut self) -> Result<Vec<String>, Error> {
        self.refuel()?;
        Ok(self
            .bindings
            .wim_plugin_events()
            .call_subscriptions(&mut self.store)?)
    }

    /// Delivers one of those events.
    pub fn on_event(&mut self, event: &Event, buffer: &Snapshot) -> Result<Edit, Error> {
        self.refuel()?;
        self.bindings
            .wim_plugin_events()
            .call_on_event(&mut self.store, event, buffer)?
            .map_err(Error::Plugin)
    }

    /// Renders the plugin's panel over `buffer`, or `None` when it has nothing to show, which is
    /// the host's cue to close the panel.
    pub fn render(&mut self, buffer: &Snapshot) -> Result<Option<Panel>, Error> {
        self.refuel()?;
        Ok(self
            .bindings
            .wim_plugin_ui()
            .call_render(&mut self.store, buffer)?)
    }

    /// Gives the call about to be made its own [`CALL_FUEL`]. Fuel is not given back when a call
    /// returns, so without this the calls of a session would share one budget and a plugin the
    /// editor keeps loaded would eventually stop for having been used rather than for looping.
    fn refuel(&mut self) -> Result<(), Error> {
        Ok(self.store.set_fuel(CALL_FUEL)?)
    }
}

/// How the engine is configured. Plugins are compiled on load rather than ahead of time, and
/// nothing beyond the component model and the fuel the store meters calls with is turned on:
/// what a plugin may do is decided by what the linker offers, and the linker offers nothing.
fn engine_config() -> Config {
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.consume_fuel(true);
    config
}

/// What a plugin's store may allocate. Only the size of a linear memory is capped; the counts
/// wasmtime limits by default (instances, tables, memories) are left where they are, since a
/// component built from this world has one of each.
fn store_limits() -> StoreLimits {
    StoreLimitsBuilder::new()
        .memory_size(MEMORY_LIMIT)
        // A refused `memory.grow` otherwise answers -1 and leaves the guest to fail however its
        // allocator happens to, which reaches the host as whatever the plugin says went wrong.
        // Trapping instead is what makes the limit an error about the limit.
        .trap_on_grow_failure(true)
        .build()
}

/// The ABI version of the wit this host was generated from, taken off the `package` line.
fn host_abi() -> &'static str {
    PLUGIN_WIT
        .lines()
        .find_map(|line| {
            line.strip_prefix("package ")?
                .strip_suffix(';')?
                .strip_prefix(PACKAGE)?
                .strip_prefix('@')
        })
        .expect("wit/plugin.wit declares a versioned package")
}

/// Refuses a component whose exports name an ABI version other than this host's.
///
/// A component carries the version in the names of the interfaces it exports
/// (`wim:plugin/commands@0.1.0`), and so do the names the bindings above look up. That is why
/// the whole version has to match: a component built against `0.1.1` exports nothing the host
/// generated from `0.1.0` can find, however little its patch changed. Reading the names first is
/// what turns that into an answer about versions instead of a wasmtime message about a missing
/// export (`wit/README.md`).
fn check_abi(engine: &Engine, component: &Component) -> Result<(), Error> {
    let expected = host_abi();
    let mut found_any = false;
    for (name, _) in component.component_type().exports(engine) {
        let Some(rest) = name.strip_prefix(PACKAGE) else {
            continue;
        };
        if !rest.starts_with('/') {
            continue;
        }
        found_any = true;
        let found = rest.split_once('@').map(|(_, version)| version);
        if found != Some(expected) {
            return Err(Error::AbiMismatch {
                expected: expected.to_string(),
                found: found.unwrap_or("(unversioned)").to_string(),
            });
        }
    }
    if found_any {
        Ok(())
    } else {
        Err(Error::NotAPlugin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_abi_is_the_one_the_wit_declares() {
        // The whole version, since the whole version is what the export names the bindings look
        // up are built out of.
        assert_eq!(host_abi(), "0.1.0");
    }

    #[test]
    fn a_core_module_is_not_a_plugin() {
        // `\0asm` and the core module version, which is what a plugin that has not been turned
        // into a component comes out as.
        let module = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        assert!(matches!(
            Plugin::from_binary(&module),
            Err(Error::NotAComponent)
        ));
    }

    #[test]
    fn a_file_that_is_not_wasm_is_not_a_plugin() {
        assert!(matches!(
            Plugin::from_binary(b"#!/bin/sh\n"),
            Err(Error::NotAComponent)
        ));
        assert!(matches!(
            Plugin::from_binary(b""),
            Err(Error::NotAComponent)
        ));
    }

    #[test]
    fn a_missing_file_is_reported_as_the_read_that_failed() {
        let Err(error) = Plugin::from_file("no/such/plugin.wasm") else {
            panic!("there is no such file");
        };
        assert!(matches!(error, Error::Io(_)), "{error}");
    }
}
