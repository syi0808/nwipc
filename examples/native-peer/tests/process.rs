use std::time::Duration;

use nwipc_process_testkit::ProcessHarness;
use nwipc_types::Generation;

fn executable() -> &'static str {
    env!("CARGO_BIN_EXE_nwipc-native-peer-example")
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
