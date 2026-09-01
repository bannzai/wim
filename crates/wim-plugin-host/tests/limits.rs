//! What a plugin may spend while it runs.
//!
//! The sandbox is about what a plugin can reach; this is about what it can use up. Both bounds
//! are on the store every call runs in, so what is fixed here is that a component doing the two
//! things an untrusted guest does to a host that runs it on the caller's thread — never
//! returning, and allocating without end — comes back as an error instead.
//!
//! The components are written in the text format for the same reason as in `loading.rs`: a
//! machine that cannot build a real plugin can still run these, and a loop that never returns is
//! easier to be sure of in four instructions than in a plugin written to misbehave.

use wim_plugin_host::{Edit, Error, Plugin, Position, Snapshot};

/// A component that exports the whole `plugin` world, with `body` as the core body of `run`.
///
/// The other five entry points trap if they are called: loading looks all six up, which is what
/// they are here for, and the tests below call `run`. Nothing reads the memory the host lowers
/// the arguments into, so one address serves every allocation.
fn plugin_running(body: &str) -> Vec<u8> {
    let wat = format!(
        r#"(component
  ;; The types of the world, in the two places a component built from the wit puts them: the ones
  ;; `buffer` defines arrive as an import, and the ones an interface defines are exported by it.
  (type $buffer (instance
    (type $position0 (record (field "line" u32) (field "column" u32)))
    (export "position" (type $position (eq $position0)))
    (type $snapshot0 (record
      (field "name" string)
      (field "text" string)
      (field "cursor" $position)))
    (export "snapshot" (type $snapshot (eq $snapshot0)))
    (type $line-edit0 (record (field "start" u32) (field "end" u32) (field "text" string)))
    (export "line-edit" (type $line-edit (eq $line-edit0)))
    (type $edit0 (variant
      (case "replace-all" string)
      (case "replace-lines" $line-edit)
      (case "message" string)
      (case "noop")))
    (export "edit" (type $edit (eq $edit0)))
  ))
  (import "wim:plugin/buffer@0.1.0" (instance $buf (type $buffer)))
  (alias export $buf "snapshot" (type $snapshot))
  (alias export $buf "edit" (type $edit))

  (core module $m
    (memory (export "memory") 1)
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
      i32.const 1024)
    (func (export "run") (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)
      (local $spin i32)
      {body})
    (func (export "list-commands") (result i32) unreachable)
    (func (export "subscriptions") (result i32) unreachable)
    (func (export "on-event") (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)
      unreachable)
    (func (export "render") (param i32 i32 i32 i32 i32 i32) (result i32) unreachable)
  )
  (core instance $i (instantiate $m))
  (alias core export $i "memory" (core memory $mem))
  (alias core export $i "cabi_realloc" (core func $realloc))
  (alias core export $i "run" (core func $run))
  (alias core export $i "list-commands" (core func $list-commands))
  (alias core export $i "subscriptions" (core func $subscriptions))
  (alias core export $i "on-event" (core func $on-event))
  (alias core export $i "render" (core func $render))

  (type $command (record (field "name" string) (field "description" string)))
  (type $event (record (field "name" string) (field "payload" string)))
  (type $panel (record (field "title" string) (field "html" string)))

  (func $lifted-list-commands (result (list $command))
    (canon lift (core func $list-commands) (memory $mem) (realloc $realloc)
      string-encoding=utf8))
  (func $lifted-run
    (param "name" string) (param "args" (list string)) (param "buf" $snapshot)
    (result (result $edit (error string)))
    (canon lift (core func $run) (memory $mem) (realloc $realloc) string-encoding=utf8))
  (func $lifted-subscriptions (result (list string))
    (canon lift (core func $subscriptions) (memory $mem) (realloc $realloc)
      string-encoding=utf8))
  (func $lifted-on-event
    (param "ev" $event) (param "buf" $snapshot)
    (result (result $edit (error string)))
    (canon lift (core func $on-event) (memory $mem) (realloc $realloc) string-encoding=utf8))
  (func $lifted-render (param "buf" $snapshot) (result (option $panel))
    (canon lift (core func $render) (memory $mem) (realloc $realloc) string-encoding=utf8))

  (instance $commands
    (export "command" (type $command))
    (export "list-commands" (func $lifted-list-commands))
    (export "run" (func $lifted-run)))
  (instance $events
    (export "event" (type $event))
    (export "subscriptions" (func $lifted-subscriptions))
    (export "on-event" (func $lifted-on-event)))
  (instance $ui
    (export "panel" (type $panel))
    (export "render" (func $lifted-render)))
  (export "wim:plugin/commands@0.1.0" (instance $commands))
  (export "wim:plugin/events@0.1.0" (instance $events))
  (export "wim:plugin/ui@0.1.0" (instance $ui))
)"#
    );
    wat::parse_str(&wat).expect("the component should assemble")
}

/// The buffer the call is made over. Small on purpose: what these tests watch is what the guest
/// spends, not what it was given.
fn buffer() -> Snapshot {
    Snapshot {
        name: "notes.txt".to_string(),
        text: "hello\n".to_string(),
        cursor: Position { line: 0, column: 0 },
    }
}

/// What a call to `run` on `body` came back with.
fn run(body: &str) -> Error {
    let wasm = plugin_running(body);
    let mut plugin = Plugin::from_binary(&wasm).expect("the component exports the whole world");
    let Err(error) = plugin.run("spin", &[], &buffer()) else {
        panic!("the call should not have finished");
    };
    error
}

#[test]
fn a_plugin_that_never_returns_is_stopped_when_its_fuel_runs_out() {
    // Counting up forever. `loop` and `br` are free, so the counter is what is metered: without
    // fuel this call never comes back and takes the editor with it.
    let forever = "(loop $again
                     (local.set $spin (i32.add (local.get $spin) (i32.const 1)))
                     (br $again))
                   unreachable";
    let error = run(forever);
    let reported = format!("{error}");
    assert!(
        matches!(error, Error::Wasm(_)) && reported.contains("all fuel consumed"),
        "the call should have run out of fuel: {reported}"
    );
}

#[test]
fn a_plugin_cannot_grow_its_memory_past_the_limit() {
    // 2048 pages is 128 MiB, which is over the limit whatever page it starts from. Growing is
    // made to trap rather than to answer -1, so reaching `unreachable` would mean it was let
    // through.
    let grow = "(drop (memory.grow (i32.const 2048)))
                unreachable";
    let error = run(grow);
    let reported = format!("{error}");
    assert!(
        matches!(error, Error::Wasm(_)) && reported.contains("growing memory"),
        "the growth should have been refused: {reported}"
    );
    assert!(
        !reported.contains("unreachable"),
        "the growth should have been refused rather than allowed: {reported}"
    );
}

#[test]
fn every_call_is_given_a_budget_of_its_own() {
    // Four fifths of one call's fuel, so that two calls cannot both come back out of a single
    // budget: the loop below spends eight fuel a turn (`local.get`, `i32.const` and `i32.add` for
    // the count, `local.set` to keep it, then `local.get`, `i32.const`, `i32.lt_u` and `br_if` to
    // go round again), which puts the last turn wasmtime lets through at 12.5 million. A plugin
    // an editor keeps loaded is called over and over, and a budget spent for good would leave it
    // stopping for having been used rather than for looping.
    let spins = 10_000_000u32;
    let wasm = plugin_running(&format!(
        "(loop $again
           (local.set $spin (i32.add (local.get $spin) (i32.const 1)))
           (br_if $again (i32.lt_u (local.get $spin) (i32.const {spins}))))
         ;; `ok(noop)`: the tag of the result, then the tag of the edit inside it.
         (i32.store8 (i32.const 64) (i32.const 0))
         (i32.store8 (i32.const 68) (i32.const 3))
         i32.const 64"
    ));
    let mut plugin = Plugin::from_binary(&wasm).expect("the component exports the whole world");
    for call in 1..=2 {
        match plugin.run("spin", &[], &buffer()) {
            Ok(edit) => assert_eq!(edit, Edit::Noop),
            Err(error) => panic!("call {call} was not given a budget of its own: {error}"),
        }
    }
}
