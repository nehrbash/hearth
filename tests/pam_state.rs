use hearth::pam::{Input, Policy, Supervisor, take_line};
use std::time::Duration;
use zeroize::Zeroizing;
#[test]
fn policies_require_explicit_bounded_service_and_deadline() {
    for service in ["", "../other", "/etc/pam.d/other", "a/b", "bad\0name"] {
        assert!(
            Policy {
                service: service.into(),
                config_dir: None,
                timeout: Duration::from_secs(1)
            }
            .validate()
            .is_err()
        );
    }
    for timeout in [0, 99, 120001] {
        assert!(
            Policy {
                service: "test".into(),
                config_dir: None,
                timeout: Duration::from_millis(timeout)
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        Policy {
            service: "hearth-test".into(),
            config_dir: None,
            timeout: Duration::from_secs(1)
        }
        .validate()
        .is_ok()
    );
}
#[test]
fn stale_commands_do_not_start_authentication() {
    let mut s = Supervisor::new(Policy {
        service: "test".into(),
        config_dir: None,
        timeout: Duration::from_secs(1),
    })
    .expect("supervisor");
    s.events.clear();
    s.command(&Input::Authenticate {
        session: "wrong".into(),
        request_id: "one".into(),
    })
    .expect("reject");
    s.command(&Input::Reply {
        session: s.session.clone(),
        attempt: "old".into(),
        prompt_id: "old".into(),
        value: "private".into(),
    })
    .expect("reject");
    assert!(s.active_fd().is_none());
    assert_eq!(s.events.len(), 2);
    let text = serde_json::to_string(&s.events).expect("events");
    assert!(!text.contains("private"));
}
#[test]
fn framing_bounds_and_preserves_partial_lines() {
    let mut data = Zeroizing::new(b"one\ntwo".to_vec());
    assert_eq!(
        &**take_line(&mut data).expect("line").expect("present"),
        b"one"
    );
    assert_eq!(&**data, b"two");
    assert!(take_line(&mut data).expect("partial").is_none());
    data.resize(16385, 0);
    assert!(take_line(&mut data).is_err());
}
