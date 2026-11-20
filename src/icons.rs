use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt, path::Path};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Icon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub fn resolve(name: &str, theme_path: Option<&str>) -> Result<Option<Icon>> {
    ensure!(
        name.len() <= 4096 && !name.contains('\0'),
        "Invalid icon name"
    );
    if name.is_empty() {
        return Ok(None);
    }
    if Path::new(name).is_absolute() {
        return load(Path::new(name)).map(Some);
    }
    ensure!(
        !name.contains('/') && name != "." && name != "..",
        "Invalid theme icon name"
    );
    if let Some(directory) = theme_path.filter(|s| !s.is_empty()) {
        ensure!(
            directory.len() <= 4096 && Path::new(directory).is_absolute(),
            "Invalid icon theme path"
        );
        for suffix in ["", ".png", ".svg"] {
            let path = Path::new(directory).join(format!("{name}{suffix}"));
            if path.is_file() {
                return load(&path).map(Some);
            }
        }
    }
    let theme = std::env::var("HEARTH_ICON_THEME").unwrap_or_else(|_| "hicolor".into());
    ensure!(
        theme.len() <= 256 && !theme.contains('/'),
        "Invalid icon theme"
    );
    freedesktop_icons::lookup(name)
        .with_size(64)
        .with_theme(&theme)
        .find()
        .map(|path| load(&path))
        .transpose()
}

pub fn load(path: &Path) -> Result<Icon> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    ensure!(file.metadata()?.is_file(), "Icon must be a regular file");
    let mut bytes = Vec::new();
    file.take(256 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 256 * 1024, "Icon exceeds 256 KiB");
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        decode_png(&bytes)
    } else {
        svg(&bytes)
    }
}

fn thumbnail(width: u32, height: u32, rgba: &[u8]) -> Icon {
    let scale = (64.0 / width.max(height) as f64).min(1.0);
    let w = ((width as f64 * scale).round() as u32).max(1);
    let h = ((height as f64 * scale).round() as u32).max(1);
    let mut result = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let index = (((y * height / h) * width + x * width / w) * 4) as usize;
            result.extend_from_slice(&rgba[index..index + 4]);
        }
    }
    Icon {
        width: w,
        height: h,
        rgba: result,
    }
}

pub fn decode_png(bytes: &[u8]) -> Result<Icon> {
    ensure!(bytes.len() <= 256 * 1024, "Icon exceeds 256 KiB");
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info()?;
    let info = reader.info();
    ensure!(
        info.width > 0
            && info.height > 0
            && info.width <= 4096
            && info.height <= 4096
            && u64::from(info.width) * u64::from(info.height) <= 4 * 1024 * 1024,
        "Icon exceeds pixel budget"
    );
    let mut data = vec![0; reader.output_buffer_size().context("Invalid PNG size")?];
    let frame = reader.next_frame(&mut data)?;
    let mut rgba = Vec::with_capacity((frame.width * frame.height * 4) as usize);
    for pixel in data[..frame.buffer_size()].chunks(frame.color_type.samples()) {
        match frame.color_type {
            png::ColorType::Rgba => rgba.extend_from_slice(pixel),
            png::ColorType::Rgb => rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]),
            png::ColorType::Grayscale => {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], 255])
            }
            png::ColorType::GrayscaleAlpha => {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]])
            }
            png::ColorType::Indexed => anyhow::bail!("Unexpanded icon palette"),
        }
    }
    Ok(thumbnail(frame.width, frame.height, &rgba))
}

fn svg(bytes: &[u8]) -> Result<Icon> {
    let text = std::str::from_utf8(bytes)?;
    // Filters can allocate intermediate surfaces larger than the icon viewport.
    ensure!(
        !text.contains("<filter") && !text.contains(":filter"),
        "SVG icon filters are unsupported"
    );
    let options = resvg::usvg::Options {
        max_depth: 32,
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_data(bytes, &options)?;
    let size = tree.size();
    ensure!(
        size.width() <= 4096.0 && size.height() <= 4096.0,
        "SVG icon exceeds dimension budget"
    );
    let scale = 64.0 / size.width().max(size.height());
    let width = (size.width() * scale).round().max(1.0) as u32;
    let height = (size.height() * scale).round().max(1.0) as u32;
    let mut pixels =
        resvg::tiny_skia::Pixmap::new(width, height).context("Invalid SVG icon size")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixels.as_mut(),
    );
    let mut rgba = pixels.data().to_vec();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = (u32::from(*channel) * 255 + alpha / 2)
                .checked_div(alpha)
                .unwrap_or(0)
                .min(255) as u8;
        }
    }
    Ok(Icon {
        width,
        height,
        rgba,
    })
}
