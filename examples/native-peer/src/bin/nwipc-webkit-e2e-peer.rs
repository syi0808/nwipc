use nwipc_peer::Peer;

fn main() {
    if let Err(error) = run() {
        eprintln!("webkit-e2e-peer: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let mut peer = Peer::initialize().map_err(|error| error.to_string())?;
    peer.run_echo().map_err(|error| error.to_string())?;
    println!("webkit-e2e-peer: production-transport=ok");
    Ok(())
}
