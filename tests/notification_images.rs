use anyhow::{Context, Result};
use hearth::{
    notification_images::{resolve, resolve_with_theme_path},
    notifications::Image,
};
use std::{
    fs,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "hearth-notification-images-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        Ok(Self(path))
    }
    fn png(&self, name: &str, pixel: [u8; 4]) -> Result<PathBuf> {
        let path = self.0.join(name);
        let mut encoder = png::Encoder::new(fs::File::create(&path)?, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&pixel)?;
        Ok(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn name(path: &Path) -> &str {
    path.to_str().expect("UTF8 fixture path")
}
#[test]
fn valid_inline_then_path_then_app_icon_preserves_alpha() -> Result<()> {
    let f = Fixture::new()?;
    let path = f.png("image.png", [20, 30, 40, 120])?;
    let app = f.png("app.png", [50, 60, 70, 180])?;
    let inline = Image {
        width: 1,
        height: 1,
        rgba: vec![1, 2, 3, 4],
    };
    assert_eq!(
        resolve(Some(&inline), Some(name(&path)), name(&app))
            .context("inline")?
            .rgba,
        inline.rgba
    );
    assert_eq!(
        resolve(None, Some(name(&path)), name(&app))
            .context("path")?
            .rgba,
        [20, 30, 40, 120]
    );
    assert_eq!(
        resolve(None, Some("/nonexistent/hearth-image"), name(&app))
            .context("app")?
            .rgba,
        [50, 60, 70, 180]
    );
    let malformed = Image {
        width: 1,
        height: 1,
        rgba: vec![],
    };
    assert_eq!(
        resolve(Some(&malformed), Some(name(&path)), "")
            .context("invalid inline fallback")?
            .rgba,
        [20, 30, 40, 120]
    );
    Ok(())
}
#[test]
fn file_uris_percent_decode_and_theme_names_resolve_locally() -> Result<()> {
    let f = Fixture::new()?;
    let path = f.png("image with # percent %.png", [1, 2, 3, 90])?;
    let uri = url::Url::from_file_path(&path).expect("file URI");
    assert_eq!(
        resolve(None, Some(uri.as_str()), "").context("URI")?.rgba,
        [1, 2, 3, 90]
    );
    let local = uri.as_str().replacen("file:///", "file://localhost/", 1);
    assert!(resolve(None, Some(&local), "").is_some());
    f.png("themed.png", [5, 6, 7, 80])?;
    assert_eq!(
        resolve_with_theme_path(None, Some("themed"), "", Some(name(&f.0)))
            .context("theme")?
            .rgba,
        [5, 6, 7, 80]
    );
    assert!(resolve_with_theme_path(None, Some("../themed"), "", Some(name(&f.0))).is_none());
    Ok(())
}
#[test]
fn remote_and_malformed_uris_fall_back_without_network_access() -> Result<()> {
    let f = Fixture::new()?;
    let app = f.png("fallback.png", [7, 8, 9, 10])?;
    for invalid in [
        "https://127.0.0.1:9/secret",
        "http://example.invalid/image.png",
        "ftp://example.invalid/image",
        "data:image/png;base64,invalid",
        "file://remote.invalid/tmp/image.png",
        "file:///tmp/image.png?query",
        "file:///tmp/image.png#fragment",
        "file:///tmp/%00image",
        "file://user@localhost/tmp/a",
        "file:%GG",
        "file://[::1]/tmp/a",
    ] {
        assert_eq!(
            resolve(None, Some(invalid), name(&app))
                .context("fallback")?
                .rgba,
            [7, 8, 9, 10],
            "{invalid}"
        );
    }
    assert!(resolve(None, None, "").is_none());
    assert!(resolve(None, Some(&"x".repeat(4097)), "").is_none());
    Ok(())
}
#[test]
fn oversized_corrupt_and_fifo_files_do_not_block_fallback() -> Result<()> {
    let f = Fixture::new()?;
    let app = f.png("fallback.png", [7, 8, 9, 10])?;
    let large = f.0.join("large.png");
    fs::write(&large, vec![0; 256 * 1024 + 1])?;
    let bad = f.0.join("bad.png");
    fs::write(&bad, b"not an image")?;
    let fifo = f.0.join("fifo");
    let cpath = std::ffi::CString::new(fifo.as_os_str().as_bytes())?;
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    for path in [&large, &bad, &fifo, &f.0] {
        let started = Instant::now();
        assert_eq!(
            resolve(None, Some(name(path)), name(&app))
                .context("file fallback")?
                .rgba,
            [7, 8, 9, 10]
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    Ok(())
}
