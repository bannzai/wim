//! `wim`: the command line front end of wim.
//!
//! `wim serve` runs the daemon of `wim-daemon` over a directory and prints what a client needs to
//! reach it: the address it took and the token it will ask for.

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

/// Runs wim's daemon.
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
through a symlink that points out of it — is refused.")]
struct Serve {
    /// Address to listen on.
    #[arg(long, value_name = "ADDR", default_value = DEFAULT_ADDR)]
    addr: SocketAddr,

    /// Directory to serve.
    #[arg(long, value_name = "DIR", default_value = DEFAULT_ROOT)]
    root: PathBuf,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve(serve) => run(serve).await,
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
    println!("listening on {addr}");
    println!("token: {}", daemon.token());
    println!("root: {}", daemon.root().display());
    // A client reads these lines while the daemon runs on, so they go out before it starts
    // serving rather than whenever the buffer happens to fill.
    if let Err(error) = io::stdout().flush() {
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
        let Command::Serve(serve) = cli.command;
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
}
