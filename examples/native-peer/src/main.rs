use nwipc_peer::Peer;

fn main() {
    match Peer::initialize().and_then(|mut peer| peer.run_echo()) {
        Ok(()) => {}
        Err(error) => {
            eprintln!("NWIPC native peer failed: {error}");
            std::process::exit(2);
        }
    }
}
