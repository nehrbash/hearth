use hearth::audio::{scaled_volumes, validate_volumes};
#[test]
fn perceptual_scaling_preserves_balance() {
    let got = scaled_volumes(&[0.125, 1.0], 0.375).expect("scale");
    assert!((got[0] - 0.015625).abs() < 1e-6);
    assert!((got[1] - 0.125).abs() < 1e-6);
    assert_eq!(
        scaled_volumes(&[0.0, 0.0], 0.5).expect("zero"),
        [0.125, 0.125]
    );
    assert_eq!(scaled_volumes(&[1.0], 0.0).expect("zero"), [0.0]);
}
#[test]
fn invalid_channel_arrays_and_targets_reject() {
    for values in [
        vec![],
        vec![f32::NAN],
        vec![-1.0],
        vec![65.0],
        vec![0.0; 65],
    ] {
        assert!(validate_volumes(&values).is_err());
    }
    for target in [-1.0, 1.1, f32::NAN] {
        assert!(scaled_volumes(&[1.0], target).is_err());
    }
}

fn snapshot() -> hearth::audio::AudioSnapshot {
    serde_json::from_value(serde_json::json!({
        "session":"test", "core_cookie":1, "devices":[], "defaults":{
            "sink":null,"source":null,"configured_sink":null,"configured_source":null
        }, "nodes":[{"id":1,"serial":2,"device_id":null,"kind":"sink",
            "name":"sink","description":null,"application_name":null,
            "state":"idle","ready":true,"channels":[1,2],
            "channel_volumes_linear":[1.0,1.0],"mute":false,
            "capabilities":{"volume":true,"mute":true,"default":true}}]
    }))
    .expect("snapshot")
}

#[test]
fn actions_require_current_ready_capable_identity() {
    use hearth::audio::{Action, ActionRequest, Kind};
    let base = snapshot();
    let mut request = ActionRequest {
        session: "test".into(),
        id: 1,
        serial: 2,
        action: Action::Volume {
            value: 0.5,
            unmute: true,
        },
    };
    assert!(base.target(&request).is_ok());
    request.session = "old".into();
    assert!(base.target(&request).is_err());
    request.session = "test".into();
    request.serial = 3;
    assert!(base.target(&request).is_err());
    request.serial = 2;
    let mut state = base.clone();
    state.nodes[0].ready = false;
    assert!(state.target(&request).is_err());
    state = base.clone();
    state.nodes[0].capabilities.mute = false;
    assert!(state.target(&request).is_err());
    state = base.clone();
    state.nodes[0].capabilities.volume = false;
    assert!(state.target(&request).is_err());
    request.action = Action::Default;
    state = base.clone();
    state.nodes[0].kind = Kind::Playback;
    assert!(state.target(&request).is_err());
}

#[test]
fn malformed_inventory_rejects_before_publication() {
    let base = snapshot();
    assert!(base.validate().is_ok());
    let mut state = base.clone();
    state.nodes.push(state.nodes[0].clone());
    assert!(state.validate().is_err());
    state = base.clone();
    state.nodes[0].state = "unknown".into();
    assert!(state.validate().is_err());
    state = base.clone();
    state.nodes[0].mute = None;
    assert!(state.validate().is_err());
    state = base.clone();
    state.nodes[0].channel_volumes_linear = None;
    assert!(state.validate().is_err());
    state = base;
    state.nodes[0].serial = None;
    assert!(state.validate().is_err());
}
