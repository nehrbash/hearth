use anyhow::Result;
use hearth::icons;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "hearth-icons-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn svg_preserves_alpha_and_ignores_external_images() -> Result<()> {
    let f = Fixture::new()?;
    let path = f.0.join("icon.svg");
    fs::write(
        &path,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="16"><rect width="32" height="16" fill="red" opacity="0.5"/><image href="file:///etc/passwd" width="32" height="16"/></svg>"#,
    )?;
    let icon = icons::load(&path)?;
    assert_eq!((icon.width, icon.height), (64, 32));
    assert!(
        icon.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[0] == 255 && p[1] == 0 && p[2] == 0 && (127..=128).contains(&p[3]))
    );
    let resolved = icons::resolve("icon", f.0.to_str())?.expect("custom icon");
    assert_eq!(icon, resolved);
    assert!(icons::resolve("../icon", f.0.to_str()).is_err());
    Ok(())
}
#[test]
fn rgb_png_is_normalized_and_scaled() -> Result<()> {
    let f = Fixture::new()?;
    let path = f.0.join("wide.png");
    let mut encoder = png::Encoder::new(fs::File::create(&path)?, 128, 64);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()?
        .write_image_data(&[20, 60, 90].repeat(128 * 64))?;
    let icon = icons::load(&path)?;
    assert_eq!((icon.width, icon.height), (64, 32));
    assert!(
        icon.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p == &[20, 60, 90, 255])
    );
    Ok(())
}
#[test]
fn malformed_oversized_and_filtered_icons_reject() -> Result<()> {
    let f = Fixture::new()?;
    let path = f.0.join("icon.svg");
    for data in [
        vec![b'x'; 256 * 1024 + 1],
        b"not an image".to_vec(),
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="5000" height="1"/>"#.to_vec(),
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><filter id="f"/></svg>"#
            .to_vec(),
    ] {
        fs::write(&path, data)?;
        assert!(icons::load(&path).is_err());
    }
    assert!(icons::load(&f.0).is_err());
    Ok(())
}
