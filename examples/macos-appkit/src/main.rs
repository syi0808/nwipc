use nwipc_appkit::AppKitAdapter;
use nwipc_macos_spi::SystemSpiProbe;

fn main() {
    let bundle = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/NWIPC.bundle".into());
    match AppKitAdapter::configure(&SystemSpiProbe, bundle, b"renderer-bootstrap") {
        Ok(_) => println!("NWIPC AppKit control plane ready"),
        Err(error) => {
            eprintln!("NWIPC AppKit unsupported: {error}");
            std::process::exit(2);
        }
    }
}
