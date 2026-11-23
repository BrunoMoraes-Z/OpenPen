//! `cargo run -p pen-update --example check [--beta] [current-version]`:
//! one real check against GitHub (or `$OPENPEN_UPDATE_URL`).

use pen_update::{Channel, Updater, Version};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let channel = if args.iter().any(|a| a == "--beta") {
        Channel::Beta
    } else {
        Channel::Stable
    };
    let current = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .and_then(|a| Version::parse(a).ok())
        .unwrap_or_else(pen_update::current_version);
    let updater = Updater::from_env().expect("updater");
    let r = pen_update::block_on(updater.check(&current, channel)).expect("runtime");
    println!(
        "{} {current} ({channel:?}): {r:#?}",
        pen_update::api_base_from_env()
    );
}
