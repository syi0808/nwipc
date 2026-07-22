use std::time::{Duration, Instant};

use nwipc_memory_api::{MappedRegion, MappingAccess, SharedMemoryProvider};
use nwipc_memory_iosurface::{IoSurfaceDescriptor, IoSurfaceProvider};
use nwipc_types::Generation;
use nwipc_webkit_testkit::{
    ECHO_DESCRIPTOR_ENV, ECHO_GENERATION, ECHO_PAYLOAD, ECHO_REGION_LENGTH, EchoState,
    decode_echo_frame, encode_echo_frame,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("webkit-e2e-peer: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let descriptor =
        std::env::var(ECHO_DESCRIPTOR_ENV).map_err(|_| format!("missing {ECHO_DESCRIPTOR_ENV}"))?;
    let descriptor = IoSurfaceDescriptor::decode(&decode_hex::<20>(&descriptor)?)
        .map_err(|error| error.to_string())?;
    let generation = Generation::new(ECHO_GENERATION).ok_or("invalid E2E generation")?;
    let provider = IoSurfaceProvider::initialize().map_err(|error| error.to_string())?;
    let mut mapping = provider
        .attach(&descriptor, generation, MappingAccess::ReadWrite)
        .map_err(|error| error.to_string())?;
    let timeout = timeout()?;
    let deadline = Instant::now() + timeout;
    let mut snapshot = [0; ECHO_REGION_LENGTH];

    loop {
        if Instant::now() >= deadline {
            return Err("renderer request timeout".into());
        }
        mapping
            .read(0, &mut snapshot)
            .map_err(|error| error.to_string())?;
        if let Ok(frame) = decode_echo_frame(&snapshot) {
            if frame.state == EchoState::RendererRequest {
                if frame.payload != ECHO_PAYLOAD {
                    return Err("renderer request mismatch".into());
                }
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    let response =
        encode_echo_frame(EchoState::PeerEcho, ECHO_PAYLOAD).map_err(|error| error.to_string())?;
    mapping
        .write(0, &response)
        .map_err(|error| error.to_string())?;

    loop {
        if Instant::now() >= deadline {
            return Err("renderer verification timeout".into());
        }
        mapping
            .read(0, &mut snapshot)
            .map_err(|error| error.to_string())?;
        if let Ok(frame) = decode_echo_frame(&snapshot) {
            if frame.state == EchoState::RendererVerified && frame.payload == ECHO_PAYLOAD {
                println!("webkit-e2e-peer: binary-echo=ok");
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn timeout() -> Result<Duration, String> {
    let seconds = std::env::var("NWIPC_E2E_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| "20".into())
        .parse::<u64>()
        .map_err(|_| "invalid NWIPC_E2E_TIMEOUT_SECONDS")?;
    if !(1..=300).contains(&seconds) {
        return Err("NWIPC_E2E_TIMEOUT_SECONDS is out of range".into());
    }
    Ok(Duration::from_secs(seconds))
}

fn decode_hex<const LENGTH: usize>(input: &str) -> Result<[u8; LENGTH], String> {
    if input.len() != LENGTH * 2 {
        return Err("invalid IOSurface descriptor length".into());
    }
    let mut output = [0; LENGTH];
    for (index, byte) in output.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&input[index * 2..index * 2 + 2], 16)
            .map_err(|_| "invalid IOSurface descriptor encoding")?;
    }
    Ok(output)
}
