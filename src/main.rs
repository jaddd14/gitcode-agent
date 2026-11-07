//! Terminal entry point for gitcode.

use gitcode::brand;

fn main() {
    let argument = std::env::args().nth(1);
    match argument.as_deref() {
        Some("--version" | "-V") => println!("{} {}", brand::NAME, env!("CARGO_PKG_VERSION")),
        _ => println!("{}: terminal coding agent (under development)", brand::NAME),
    }
}
