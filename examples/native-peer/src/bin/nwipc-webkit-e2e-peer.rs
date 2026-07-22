use nwipc_peer::Peer;
use nwipc_peer_core::PortEvent;

fn main() {
    if let Err(error) = run() {
        eprintln!("webkit-e2e-peer: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let mut peer = Peer::initialize().map_err(|error| error.to_string())?;
    match std::env::var("NWIPC_WEBKIT_E2E_PEER_MODE").as_deref() {
        Ok("writer-before-commit") => return verify_uncommitted_is_hidden(&mut peer),
        Ok("writer-after-commit") => return verify_committed_is_visible(&mut peer),
        Ok("peer-kill") => terminate_after_handshake(),
        Ok(_) | Err(_) => {}
    }
    peer.run_echo().map_err(|error| error.to_string())?;
    println!("webkit-e2e-peer: production-transport=ok");
    Ok(())
}

fn terminate_after_handshake() -> ! {
    use std::io::Write as _;

    println!("webkit-e2e-peer: handshake-before-kill=ok");
    let _ = std::io::stdout().flush();
    std::process::abort()
}

fn verify_uncommitted_is_hidden(peer: &mut Peer) -> Result<(), String> {
    use std::io::Write as _;

    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(2));
        println!("webkit-e2e-peer: writer-before-commit-hidden=ok");
        let _ = std::io::stdout().flush();
        std::process::exit(0);
    });
    match peer.try_receive().map_err(|error| error.to_string())? {
        Some(PortEvent::Message(_)) => Err("uncommitted writer bytes became visible".into()),
        Some(PortEvent::Closed) | None => Err("writer closed before visibility window".into()),
    }
}

fn verify_committed_is_visible(peer: &mut Peer) -> Result<(), String> {
    match peer.try_receive().map_err(|error| error.to_string())? {
        Some(PortEvent::Message(payload)) if payload.len() == 257 => {
            println!("webkit-e2e-peer: writer-after-commit-visible=ok");
            Ok(())
        }
        Some(PortEvent::Message(_)) => Err("committed writer payload mismatch".into()),
        Some(PortEvent::Closed) | None => Err("committed writer payload was not visible".into()),
    }
}
