use nwipc_peer::Peer;

fn main() {
    match Peer::initialize() {
        Ok(_) => println!("NWIPC native peer initialized"),
        Err(error) => {
            eprintln!("NWIPC native peer unavailable: {error}");
            std::process::exit(2);
        }
    }
}
