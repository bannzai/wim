//! Modal editing core of wim.
//!
//! This crate must stay free of file IO, rendering and platform dependencies so that it
//! keeps compiling for `wasm32-unknown-unknown` as well as native targets.

/// Version of this crate, exposed so that frontends can report the core they were built against.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_not_empty() {
        assert!(!version().is_empty());
    }
}
