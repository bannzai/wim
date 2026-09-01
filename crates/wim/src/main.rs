//! `wim`: the command line front end of wim.
//!
//! `wim serve` runs the daemon of `wim-daemon` over a directory and prints what a client needs to
//! reach it: the address it took and the token it will ask for. `wim plugin` runs a plugin
//! against a buffer read from standard input, which is how a `.wasm` is checked without an
//! editor around it. `wim edit` types keys at a file with the autocmds of a `wim.jsonc` wired up,
//! which is the native host of `documents/CONFIG.md`.

mod config;
mod edit;
mod plugin;

use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use wim_daemon::Daemon;

/// The name errors are reported under, and the name of the binary.
const PROGRAM: &str = "wim";

/// Loopback, so that only this machine can reach the daemon and the token is all that stands
/// between it and the other processes here; remote use goes through an SSH tunnel
/// (`documents/adr/0001-daemon-fs-provider.md`). Port 0 lets the operating system pick a free
/// port, so that several daemons can run at once without being told which ports are taken; the
/// one it picked is printed.
const DEFAULT_ADDR: &str = "127.0.0.1:0";

/// The directory the daemon was started in, so that `wim serve` inside a project serves that
/// project without naming it twice.
const DEFAULT_ROOT: &str = ".";

/// Runs wim's daemon and its plugins.
#[derive(Debug, Parser)]
#[command(name = PROGRAM, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve a directory to wim clients over WebSocket.
    Serve(Serve),
    /// Run a wasm plugin over a buffer.
    Plugin(plugin::Plugin),
    /// Type keys at a file, running the autocmds a config declares.
    Edit(edit::Edit),
}

#[derive(Debug, Args)]
#[command(long_about = "\
Serves a directory to wim clients over WebSocket.

The daemon lists, reads and writes the files under --root and nothing else: the buffer being
edited lives in the client, not here.

On startup the address it took and the token it will ask for are printed, one per line, and a
client presents that token in the first message it sends. A connection that opens with anything
else is dropped.

A path a request names is read from --root, and one that reaches outside it — with '..', or
through a symlink that points out of it — is refused.

--addr has to name a loopback address: the daemon speaks plain WebSocket, so reaching it from
another machine goes through an SSH tunnel.")]
struct Serve {
    /// Address to listen on. Loopback only.
    #[arg(long, value_name = "ADDR", default_value = DEFAULT_ADDR, value_parser = loopback_addr)]
    addr: SocketAddr,

    /// Directory to serve.
    #[arg(long, value_name = "DIR", default_value = DEFAULT_ROOT)]
    root: PathBuf,
}

/// `text` as an address to listen on, which has to be one only this machine can reach.
///
/// The daemon carries the token in an `auth` message over a WebSocket it does not encrypt, so an
/// address another machine can reach would put the token, and then the contents of every file
/// under --root, on the network in the clear. Phase 2 is loopback and an SSH tunnel for anything
/// beyond it (`documents/adr/0001-daemon-fs-provider.md`), and an address that is not loopback is
/// refused here rather than served.
fn loopback_addr(text: &str) -> Result<SocketAddr, String> {
    let addr: SocketAddr = text.parse().map_err(|error| format!("{error}"))?;
    if addr.ip().is_loopback() {
        Ok(addr)
    } else {
        Err(format!(
            "{addr} is not on loopback: wim serves over a WebSocket it does not encrypt, and is \
             reached from another machine through an SSH tunnel"
        ))
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve(serve) => run(serve).await,
        // Running a plugin is one call into wasmtime and then the answer, with nothing else to
        // wait on, so it is done on this thread rather than handed to the runtime.
        Command::Plugin(plugin) => plugin::main(plugin),
        // A run reads a file, types keys at it and writes it back, all of it in step: there is
        // nothing to await either.
        Command::Edit(edit) => edit::main(edit),
    }
}

/// Starts the daemon, reports how to reach it, and serves until something goes wrong.
async fn run(serve: Serve) -> ExitCode {
    let daemon = match Daemon::bind(serve.addr, &serve.root).await {
        Ok(daemon) => daemon,
        Err(error) => {
            eprintln!(
                "{PROGRAM}: cannot serve {} on {}: {error}",
                serve.root.display(),
                serve.addr
            );
            return ExitCode::FAILURE;
        }
    };
    let addr = match daemon.local_addr() {
        Ok(addr) => addr,
        Err(error) => {
            eprintln!("{PROGRAM}: cannot tell which address was taken: {error}");
            return ExitCode::FAILURE;
        }
    };
    // A client reads these lines while the daemon runs on, so they go out before it starts
    // serving rather than whenever the buffer happens to fill. These lines are the one place
    // the token is disclosed, so a daemon that could not get them out is unreachable and ends
    // as a failure its caller can see — through this branch rather than through the panic
    // `println!` makes of a reader that went away mid-report.
    let report = {
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "listening on {addr}")
            .and_then(|()| writeln!(stdout, "token: {}", daemon.token()))
            .and_then(|()| writeln!(stdout, "root: {}", daemon.root().display()))
            .and_then(|()| stdout.flush())
    };
    if let Err(error) = report {
        eprintln!("{PROGRAM}: cannot report how to reach the daemon: {error}");
        return ExitCode::FAILURE;
    }
    if let Err(error) = daemon.serve().await {
        eprintln!("{PROGRAM}: {addr}: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serve(arguments: &[&str]) -> Serve {
        let cli = Cli::try_parse_from(
            [PROGRAM, "serve"]
                .into_iter()
                .chain(arguments.iter().copied()),
        )
        .expect("arguments should parse");
        let Command::Serve(serve) = cli.command else {
            panic!("`serve` should parse as `serve`");
        };
        serve
    }

    #[test]
    fn serve_listens_on_loopback_over_the_current_directory_by_default() {
        let serve = serve(&[]);
        assert!(serve.addr.ip().is_loopback());
        assert_eq!(serve.addr.port(), 0, "the operating system picks the port");
        assert_eq!(serve.root, PathBuf::from("."));
    }

    #[test]
    fn serve_takes_the_address_and_the_directory_it_is_given() {
        let serve = serve(&["--addr", "127.0.0.1:7777", "--root", "/tmp/project"]);
        assert_eq!(serve.addr, "127.0.0.1:7777".parse::<SocketAddr>().unwrap());
        assert_eq!(serve.root, PathBuf::from("/tmp/project"));
    }

    #[test]
    fn an_address_that_is_not_one_is_refused_before_anything_is_served() {
        assert!(Cli::try_parse_from([PROGRAM, "serve", "--addr", "127.0.0.1"]).is_err());
    }

    #[test]
    fn an_address_other_machines_could_reach_is_refused_before_anything_is_served() {
        for addr in ["0.0.0.0:7777", "192.168.1.2:7777", "[::]:7777"] {
            assert!(
                Cli::try_parse_from([PROGRAM, "serve", "--addr", addr]).is_err(),
                "{addr}"
            );
        }
    }

    #[test]
    fn loopback_is_served_whichever_of_its_addresses_is_asked_for() {
        for addr in ["127.0.0.1:7777", "127.0.0.2:7777", "[::1]:7777"] {
            assert_eq!(
                serve(&["--addr", addr]).addr,
                addr.parse::<SocketAddr>().expect("the address is one"),
                "{addr}"
            );
        }
    }
}
