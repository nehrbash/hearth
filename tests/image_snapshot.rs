use anyhow::Result;
use hearth::{
    gpu::Gpu,
    render,
    scene::{Bounds, ImageAsset, LayerEffect, Node, Scene, rect},
};
use std::path::Path;

fn asset() -> Result<ImageAsset> {
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 4, 2);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&[
            255, 0, 0, 255, 255, 0, 0, 255, 0, 255, 0, 128, 0, 255, 0, 128, 0, 0, 255, 255, 0, 0,
            255, 255, 255, 255, 255, 0, 255, 255, 255, 0,
        ])?;
    }
    ImageAsset::from_png(&png)
}

fn image(asset: &ImageAsset, bounds: Bounds, source: Bounds, opacity: f32) -> Node {
    Node::Image {
        bounds,
        source,
        image: asset.clone(),
        opacity,
    }
}

#[test]
fn atlas_alpha_clipping_and_order_match_snapshot() -> Result<()> {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    let art = asset()?;
    let scene = Scene::new(vec![
        rect(0.0, 0.0, 320.0, 200.0, [0.08, 0.10, 0.16, 1.0]),
        image(
            &art,
            Bounds::new(10.0, 10.0, 80.0, 80.0),
            Bounds::new(0.0, 0.0, 2.0, 1.0),
            1.0,
        ),
        image(
            &art,
            Bounds::new(110.0, 10.0, 80.0, 80.0),
            Bounds::new(2.0, 0.0, 2.0, 1.0),
            1.0,
        ),
        rect(210.0, 10.0, 80.0, 80.0, [0.0, 1.0, 0.0, 128.0 / 255.0]),
        Node::Group {
            offset: [10.0, 110.0],
            children: vec![Node::Clip {
                bounds: Bounds::new(10.0, 10.0, 60.0, 60.0),
                children: vec![image(
                    &art,
                    Bounds::new(0.0, 0.0, 80.0, 80.0),
                    Bounds::new(0.0, 1.0, 2.0, 1.0),
                    1.0,
                )],
            }],
        },
        image(
            &art,
            Bounds::new(110.0, 110.0, 80.0, 80.0),
            Bounds::new(2.0, 1.0, 2.0, 1.0),
            1.0,
        ),
        Node::Layer {
            bounds: Bounds::new(210.0, 110.0, 80.0, 80.0),
            effect: LayerEffect::None,
            children: vec![image(
                &art,
                Bounds::new(0.0, 0.0, 80.0, 80.0),
                Bounds::new(2.0, 0.0, 2.0, 1.0),
                1.0,
            )],
        },
        rect(10.0, 40.0, 80.0, 8.0, [1.0, 1.0, 0.0, 1.0]),
    ])?;
    let pixels = render::offscreen(&gpu, &scene, 320, 200)?;
    let pixel = |x: usize, y: usize| &pixels[(y * 320 + x) * 4..(y * 320 + x) * 4 + 4];
    assert_eq!(pixel(30, 30), &[255, 0, 0, 255]);
    assert_eq!(pixel(30, 43), &[255, 255, 0, 255]);
    assert_eq!(pixel(150, 150), pixel(0, 0));
    assert_eq!(pixel(15, 115), pixel(0, 0));
    assert_eq!(pixel(30, 130), &[0, 0, 255, 255]);
    for (a, b) in pixel(150, 50).iter().zip(pixel(250, 50)) {
        assert!(a.abs_diff(*b) <= 1);
    }
    for (a, b) in pixel(250, 150).iter().zip(pixel(150, 50)) {
        assert!(a.abs_diff(*b) <= 1);
    }
    let baseline = Path::new(env!("CARGO_MANIFEST_DIR")).join("snapshots/image-primitives.png");
    if std::env::var_os("HEARTH_UPDATE_IMAGE_SNAPSHOT").is_some() {
        render::write_png(&baseline, 320, 200, &pixels)?;
    }
    let (width, height, expected) = render::read_png(&baseline)?;
    assert_eq!((width, height), (320, 200));
    assert_eq!(pixels, expected);
    Ok(())
}

#[test]
fn minification_averages_detail_without_neighboring_atlas_colors() -> Result<()> {
    let mut pixels = Vec::new();
    for y in 0..32 {
        for x in 0..64 {
            pixels.extend_from_slice(if x >= 32 {
                &[0, 0, 255, 255]
            } else if (x + y) % 2 == 0 {
                &[255, 0, 0, 255]
            } else {
                &[0, 0, 0, 255]
            });
        }
    }
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 64, 32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&pixels)?;
    }
    let art = ImageAsset::from_png(&png)?;
    let scene = Scene::new(vec![image(
        &art,
        Bounds::new(0.0, 0.0, 4.0, 4.0),
        Bounds::new(0.0, 0.0, 32.0, 32.0),
        1.0,
    )])?;
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    let pixels = render::offscreen(&gpu, &scene, 4, 4)?;
    for rgba in pixels.as_chunks::<4>().0 {
        assert!(
            (186..=190).contains(&rgba[0]),
            "detail did not average: {rgba:?}"
        );
        assert_eq!(&rgba[1..], &[0, 0, 255]);
    }
    Ok(())
}
