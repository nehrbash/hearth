use crate::{icons, notifications::Image};
use std::path::Path;
use url::Url;

pub fn resolve(image: Option<&Image>, image_path: Option<&str>, app_icon: &str) -> Option<Image> {
    resolve_with_theme_path(image, image_path, app_icon, None)
}

pub fn resolve_with_theme_path(
    image: Option<&Image>,
    image_path: Option<&str>,
    app_icon: &str,
    theme_path: Option<&str>,
) -> Option<Image> {
    image
        .filter(|image| valid_image(image))
        .cloned()
        .or_else(|| image_path.and_then(|path| reference(path, theme_path)))
        .or_else(|| reference(app_icon, theme_path))
}

fn valid_image(image: &Image) -> bool {
    image.width > 0
        && image.height > 0
        && image.width <= 512
        && image.height <= 512
        && image.rgba.len() == image.width as usize * image.height as usize * 4
}

fn reference(value: &str, theme_path: Option<&str>) -> Option<Image> {
    if value.is_empty() || value.len() > 4096 || value.contains('\0') {
        return None;
    }
    let icon = if Path::new(value).is_absolute() {
        icons::load(Path::new(value)).ok()?
    } else if let Ok(uri) = Url::parse(value) {
        if uri.scheme() != "file"
            || uri.query().is_some()
            || uri.fragment().is_some()
            || !uri.username().is_empty()
            || uri.password().is_some()
            || uri.port().is_some()
            || uri.host_str().is_some_and(|host| host != "localhost")
        {
            return None;
        }
        icons::load(&uri.to_file_path().ok()?).ok()?
    } else {
        if value.contains(':') {
            return None;
        }
        icons::resolve(value, theme_path).ok()??
    };
    Some(Image {
        width: icon.width,
        height: icon.height,
        rgba: icon.rgba,
    })
}
