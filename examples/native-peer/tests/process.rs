use std::time::Duration;

use nwipc_process_testkit::ProcessHarness;
use nwipc_types::Generation;

fn executable() -> &'static str {
    env!("CARGO_BIN_EXE_nwipc-native-peer-example")
}

#[cfg(target_os = "macos")]
fn wait_for_success(child: &mut std::process::Child, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "native peer did not exit"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn native_child_echoes_binary_and_closes() {
    let harness = ProcessHarness::new(Duration::from_secs(2));
    let mut peer = harness.spawn(executable()).unwrap();
    assert_eq!(peer.echo(&[0, 1, 0xff, 2]).unwrap(), [0, 1, 0xff, 2]);
    assert!(peer.close().unwrap().success());
}

#[test]
fn stale_generation_is_rejected_before_handshake() {
    let harness = ProcessHarness::new(Duration::from_secs(2));
    assert!(
        harness
            .spawn_with_expected_generation(executable(), Generation::new(2).unwrap())
            .is_err()
    );
}

#[test]
fn killed_child_is_reaped() {
    let harness = ProcessHarness::new(Duration::from_secs(2));
    let peer = harness.spawn(executable()).unwrap();
    assert!(!peer.kill().unwrap().success());
}

#[test]
fn stress_echo_and_process_replacement_do_not_deliver_stale_data() {
    let harness = ProcessHarness::new(Duration::from_secs(2));
    let mut previous_session = None;
    for replacement in 0_u8..6 {
        let mut peer = harness.spawn(executable()).unwrap();
        let session = peer.expectation().session_id;
        assert_ne!(previous_session, Some(session));
        for sequence in 0_u16..128 {
            let mut payload = sequence.to_le_bytes().to_vec();
            payload.resize(usize::from(sequence % 97) + 2, replacement);
            assert_eq!(peer.echo(&payload).unwrap(), payload);
        }
        previous_session = Some(session);
        if replacement % 2 == 0 {
            assert!(!peer.kill().unwrap().success());
        } else {
            assert!(peer.close().unwrap().success());
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn public_endpoints_use_bootstrap_pipe_only_for_production_echo() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    use nwipc::Nwipc;
    use nwipc_renderer_api::{SendDisposition, TransportEvent};

    let mut nwipc = Nwipc::initialize().unwrap();
    let mut session = nwipc.create_session().unwrap();
    let mut command = Command::new(executable());
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    session.peer_environment().apply(&mut command);
    let mut child = command.spawn().unwrap();
    session
        .write_peer_bootstrap(child.stdin.as_mut().unwrap())
        .unwrap();
    child.stdin.take().unwrap().flush().unwrap();

    let mut renderer = nwipc.open_renderer(&mut session).unwrap();
    assert_eq!(
        renderer.send(&[0, 1, 0xff, 2]).unwrap(),
        SendDisposition::Sent
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match renderer.poll().unwrap() {
            Some(TransportEvent::Message(payload)) => {
                assert_eq!(payload, [0, 1, 0xff, 2]);
                break;
            }
            Some(event) => panic!("unexpected renderer event: {event:?}"),
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
            None => panic!("production peer echo timed out"),
        }
    }
    renderer.close().unwrap();
    wait_for_success(&mut child, Duration::from_secs(2));
    nwipc.close(&session).unwrap();
}
