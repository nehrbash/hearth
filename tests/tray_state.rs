use hearth::tray::{ActionRequest, argb_icon};
#[test]
fn pixmap_bytes_are_network_argb_and_downsample_is_bounded() {
    let image = argb_icon(2, 1, &[128, 255, 0, 10, 255, 0, 20, 255]).expect("image");
    assert_eq!(image.rgba, [255, 0, 10, 128, 0, 20, 255, 255]);
    let image = argb_icon(256, 128, &vec![255; 256 * 128 * 4]).expect("large image");
    assert_eq!(
        (image.width, image.height, image.rgba.len()),
        (64, 32, 64 * 32 * 4)
    );
}
#[test]
fn malformed_pixmaps_fail_before_allocation() {
    for (w, h, len) in [
        (0, 2, 0),
        (-1, 2, 0),
        (257, 1, 1028),
        (2, 2, 15),
        (i32::MAX, i32::MAX, 0),
    ] {
        assert!(argb_icon(w, h, &vec![0; len]).is_err());
    }
}
#[test]
fn commands_require_explicit_identity_and_typed_arguments() {
    let request = serde_json::json!({"session":"a","service":"org.example.Item","owner":":1.2","path":"/StatusNotifierItem","action":{"action":"menu_event","epoch":2,"revision":7,"id":1,"event":"clicked","timestamp":0}});
    assert!(serde_json::from_value::<ActionRequest>(request.clone()).is_ok());
    let mut invalid = request.clone();
    invalid["action"]["revision"] = serde_json::json!(-1);
    assert!(serde_json::from_value::<ActionRequest>(invalid).is_err());
    let mut invalid = request.clone();
    invalid.as_object_mut().expect("object").remove("owner");
    assert!(serde_json::from_value::<ActionRequest>(invalid).is_err());
    let mut invalid = request;
    invalid["action"]["unchecked"] = true.into();
    assert!(serde_json::from_value::<ActionRequest>(invalid).is_err());
}
