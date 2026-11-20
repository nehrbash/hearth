use anyhow::Result;
use hearth::{
    animation::Seconds,
    gpu::Gpu,
    interactive::{InteractiveScene, PointerKind},
    render,
};
use serde_json::{Value, json};
use std::path::Path;
fn toggle() -> Vec<Value> {
    vec![
        json!({"key":"canvas","kind":"rect","bounds":[0,0,400,240],"color":[0.025,0.035,0.06,1]}),
        json!({"key":"title","kind":"text","bounds":[32,28,340,36],"text":"Scheme owns the toggle","font_size":24,"color":[0.92,0.89,0.8,1]}),
        json!({"key":"toggle","kind":"group","bounds":[0,0,336,70],"offset":[32,92],"input":true,"states":{"hover":{"offset":[32,94]},"pressed":{"offset":[32,98]}},"transitions":{"offset":100}}),
        json!({"key":"surface","parent":"toggle","kind":"rect","bounds":[0,0,336,70],"color":[0.12,0.16,0.24,1],"state":"off","states":{"on":{"color":[0.08,0.34,0.26,1]}},"transitions":{"color":200}}),
        json!({"key":"label","parent":"toggle","kind":"text","bounds":[20,20,296,32],"text":"Off · click to enable","font_size":21,"color":[0.95,0.92,0.82,1]}),
        json!({"key":"hint","kind":"text","bounds":[32,190,340,28],"text":"Rust owns hover, press and animation","font_size":16,"color":[0.62,0.69,0.77,1]}),
    ]
}
fn pixels(gpu: &Gpu, scene: &InteractiveScene) -> Result<Vec<u8>> {
    render::offscreen(gpu, &scene.scene()?, 400, 240)
}
fn snapshot(name: &str, pixels: &[u8]) -> Result<()> {
    let path = format!("snapshots/interactive-{name}.png");
    if std::env::var_os("HEARTH_UPDATE_SNAPSHOTS").is_some() {
        render::write_png(Path::new(&path), 400, 240, pixels)?;
    }
    let (width, height, expected) = render::read_png(Path::new(&path))?;
    assert_eq!((width, height), (400, 240));
    assert!(pixels == expected, "Interactive {name} PNG differs");
    Ok(())
}
#[test]
fn toggle_wire_state_and_local_press_produce_distinct_frames() -> Result<()> {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    let mut scene = InteractiveScene::new();
    let mut nodes = toggle();
    scene.apply(&nodes)?;
    let off = pixels(&gpu, &scene)?;
    snapshot("off", &off)?;
    scene.pointer(PointerKind::Press, 100.0, 120.0);
    scene.advance(Seconds::new(0.05)?);
    let pressed = pixels(&gpu, &scene)?;
    snapshot("pressed", &pressed)?;
    assert_ne!(pressed, off);
    assert!(
        scene
            .pointer(PointerKind::Release, 100.0, 120.0)
            .iter()
            .any(|e| e.key == "toggle" && e.event == "activate")
    );
    nodes[3]["state"] = json!("on");
    nodes[4]["text"] = json!("On · click to disable");
    scene.apply(&nodes)?;
    scene.pointer(PointerKind::Leave, 0.0, 0.0);
    scene.advance(Seconds::new(0.1)?);
    let middle = pixels(&gpu, &scene)?;
    snapshot("mid", &middle)?;
    assert!(scene.is_active());
    scene.advance(Seconds::new(0.1)?);
    let on = pixels(&gpu, &scene)?;
    snapshot("on", &on)?;
    assert_ne!(middle, off);
    assert_ne!(middle, on);
    assert_ne!(on, off);
    assert!(!scene.is_active());
    Ok(())
}
