//! What the host refuses to load.
//!
//! The components here are written in the text format and hold no code: what is being fixed is
//! the shape of a component's imports and exports, which is all the host looks at before it runs
//! anything. Writing them by hand is also what lets these run on a machine that cannot build a
//! real plugin, since building one needs the `wasm32-wasip2` target (`wit/README.md`).

use wim_plugin_host::{Error, Plugin};

/// Assembles a component out of the items given in the text format.
fn component(items: &str) -> Vec<u8> {
    wat::parse_str(format!("(component {items})")).expect("the component should assemble")
}

/// Exports of the world's three interfaces at `version`, with nothing in them. A component made
/// of these gets as far as the ABI check, which is all these tests need.
fn exports_at(version: &str) -> String {
    format!(
        r#"(instance $empty)
           (export "wim:plugin/commands@{version}" (instance $empty))
           (export "wim:plugin/events@{version}" (instance $empty))
           (export "wim:plugin/ui@{version}" (instance $empty))"#
    )
}

#[test]
fn a_component_that_wants_wasi_cannot_be_loaded() {
    // Listing the directories it may open is `wasi:filesystem`, and the host defines nothing for
    // it. The import is what makes this fail: an empty linker is the sandbox, so a plugin that
    // asks for anything at all is turned away before it is instantiated.
    let wasm = component(&format!(
        r#"(import "wasi:filesystem/preopens@0.2.0" (instance
             (export "get-directories" (func (result (list u32))))
           ))
           {}"#,
        exports_at("0.1.0")
    ));
    let Err(error) = Plugin::from_binary(&wasm) else {
        panic!("a component importing wasi should not load");
    };
    assert!(
        matches!(error, Error::Wasm(_)),
        "the failure should come from wasmtime: {error}"
    );
    let reported = format!("{error}");
    assert!(
        reported.contains("wasi:filesystem/preopens@0.2.0"),
        "the interface that was refused should be named: {reported}"
    );
}

#[test]
fn the_one_interface_the_world_imports_needs_nothing_from_the_host() {
    // `wim:plugin/buffer` carries types and no functions, so a plugin importing it asks the host
    // for nothing and the empty linker still instantiates it. This is why the sandbox can be an
    // empty linker rather than a set of host functions that refuse to do anything.
    let wasm = component(&format!(
        r#"(import "wim:plugin/buffer@0.1.0" (instance
             (type $position (record (field "line" u32) (field "column" u32)))
             (export "position" (type (eq $position)))
           ))
           {}"#,
        exports_at("0.1.0")
    ));
    let Err(error) = Plugin::from_binary(&wasm) else {
        panic!("the empty instances are not the world's interfaces");
    };
    let reported = format!("{error}");
    assert!(
        !reported.contains("wim:plugin/buffer"),
        "the buffer import should not have to be satisfied: {reported}"
    );
}

#[test]
fn a_component_built_against_another_abi_is_refused() {
    let wasm = component(&exports_at("0.2.0"));
    let Err(Error::AbiMismatch { expected, found }) = Plugin::from_binary(&wasm) else {
        panic!("a component built against 0.2.0 should be refused");
    };
    assert_eq!(expected, "0.1.0");
    assert_eq!(found, "0.2.0");
}

#[test]
fn an_abi_that_differs_only_in_its_patch_is_refused_too() {
    // The patch digit marks changes that do not touch the ABI, but it is part of the export names
    // all the same, so a component built against 0.1.7 exports nothing this host can find. Saying
    // so as a version mismatch is the point: letting it past here would only move the refusal to
    // a wasmtime message about a missing export (`wit/README.md`).
    let wasm = component(&exports_at("0.1.7"));
    let Err(Error::AbiMismatch { expected, found }) = Plugin::from_binary(&wasm) else {
        panic!("a component built against 0.1.7 should be refused");
    };
    assert_eq!(expected, "0.1.0");
    assert_eq!(found, "0.1.7");
}

#[test]
fn a_component_that_is_not_a_plugin_is_refused() {
    assert!(matches!(
        Plugin::from_binary(&component("")),
        Err(Error::NotAPlugin)
    ));
}
