use crate::{
    gpu::Gpu,
    scene::{Bounds, PaintOp, Quad, Scene},
};
use anyhow::{Context, Result};
use std::{
    fs::File,
    io::{BufReader, BufWriter},
    path::Path,
};
use wgpu::util::DeviceExt;

pub const BACKGROUND: wgpu::Color = wgpu::Color {
    r: 0.025,
    g: 0.035,
    b: 0.06,
    a: 1.0,
};
pub const SNAPSHOT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 2],
    color: [f32; 4],
}

pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    text: Option<crate::text::TextSystem>,
    format: wgpu::TextureFormat,
}

impl Renderer {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        let shader = gpu
            .device
            .create_shader_module(wgpu::include_wgsl!("../shaders/scene.wgsl"));
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Hearth scene"),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Vertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4],
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fragment"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            });
        Self {
            pipeline,
            text: None,
            format,
        }
    }

    pub fn draw(
        &mut self,
        gpu: &Gpu,
        view: &wgpu::TextureView,
        scene: &Scene,
        size: [u32; 2],
    ) -> Result<()> {
        anyhow::ensure!(
            size[0] > 0 && size[1] > 0,
            "Render dimensions must be positive"
        );
        let operations = scene.paint_ops(Bounds::new(0.0, 0.0, size[0] as f32, size[1] as f32));
        self.draw_quads(gpu, view, &[], size, wgpu::LoadOp::Clear(BACKGROUND));
        let mut remaining = operations.as_slice();
        while let Some(first) = remaining.first() {
            let text_run = matches!(first, PaintOp::Text(_));
            let count = remaining
                .iter()
                .take_while(|op| matches!(op, PaintOp::Text(_)) == text_run)
                .count();
            let (run, rest) = remaining.split_at(count);
            if text_run {
                let labels: Vec<_> = run
                    .iter()
                    .filter_map(|op| match op {
                        PaintOp::Text(label) => Some(label.clone()),
                        _ => None,
                    })
                    .collect();
                let text = self
                    .text
                    .get_or_insert_with(|| crate::text::TextSystem::new(gpu, self.format));
                text.prepare(gpu, size, &labels)?;
                let mut encoder = gpu.device.create_command_encoder(&Default::default());
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Scene text"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    text.render(&mut pass)?;
                }
                gpu.queue.submit([encoder.finish()]);
            } else {
                let quads: Vec<_> = run
                    .iter()
                    .filter_map(|op| match op {
                        PaintOp::Rect(quad) => Some(*quad),
                        _ => None,
                    })
                    .collect();
                self.draw_quads(gpu, view, &quads, size, wgpu::LoadOp::Load);
            }
            remaining = rest;
        }
        Ok(())
    }

    fn draw_quads(
        &self,
        gpu: &Gpu,
        view: &wgpu::TextureView,
        quads: &[Quad],
        size: [u32; 2],
        load: wgpu::LoadOp<wgpu::Color>,
    ) {
        let mut vertices = Vec::new();
        for quad in quads {
            let b = quad.bounds;
            let x0 = b.x / size[0] as f32 * 2.0 - 1.0;
            let x1 = (b.x + b.width) / size[0] as f32 * 2.0 - 1.0;
            let y0 = 1.0 - b.y / size[1] as f32 * 2.0;
            let y1 = 1.0 - (b.y + b.height) / size[1] as f32 * 2.0;
            for position in [[x0, y0], [x0, y1], [x1, y1], [x0, y0], [x1, y1], [x1, y0]] {
                vertices.push(Vertex {
                    position,
                    color: quad.color,
                });
            }
        }
        let buffer = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Scene vertices"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Scene render"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if !vertices.is_empty() {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..vertices.len() as u32, 0..1);
            }
        }
        gpu.queue.submit([encoder.finish()]);
    }
}

pub struct PanelRenderer {
    geometry: Renderer,
    text: crate::text::TextSystem,
}

impl PanelRenderer {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        Self {
            geometry: Renderer::new(gpu, format),
            text: crate::text::TextSystem::new(gpu, format),
        }
    }

    pub fn draw(
        &mut self,
        gpu: &Gpu,
        view: &wgpu::TextureView,
        panel: &crate::panel::Panel,
        size: [u32; 2],
    ) -> Result<()> {
        self.text.prepare(gpu, size, &panel.labels)?;
        self.geometry.draw(gpu, view, &panel.scene, size)?;
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Panel text"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.text.render(&mut pass)?;
        }
        gpu.queue.submit([encoder.finish()]);
        Ok(())
    }
}

fn snapshot_texture(gpu: &Gpu, width: u32, height: u32) -> Result<wgpu::Texture> {
    anyhow::ensure!(
        width > 0 && height > 0 && width <= 8192 && height <= 8192,
        "Invalid snapshot size"
    );
    Ok(gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Snapshot"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: SNAPSHOT_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    }))
}

pub fn offscreen(gpu: &Gpu, scene: &Scene, width: u32, height: u32) -> Result<Vec<u8>> {
    let texture = snapshot_texture(gpu, width, height)?;
    Renderer::new(gpu, SNAPSHOT_FORMAT).draw(
        gpu,
        &texture.create_view(&Default::default()),
        scene,
        [width, height],
    )?;
    read_texture(gpu, &texture, width, height)
}

pub fn offscreen_panel(
    gpu: &Gpu,
    panel: &crate::panel::Panel,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    let texture = snapshot_texture(gpu, width, height)?;
    PanelRenderer::new(gpu, SNAPSHOT_FORMAT).draw(
        gpu,
        &texture.create_view(&Default::default()),
        panel,
        [width, height],
    )?;
    read_texture(gpu, &texture, width, height)
}

fn read_texture(gpu: &Gpu, texture: &wgpu::Texture, width: u32, height: u32) -> Result<Vec<u8>> {
    let stride = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Snapshot readback"),
        size: u64::from(stride) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(height),
            },
        },
        texture.size(),
    );
    gpu.queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
    gpu.device.poll(wgpu::PollType::wait_indefinitely())?;
    receiver
        .recv()
        .context("GPU readback callback disappeared")??;
    let mapped = buffer.slice(..).get_mapped_range();
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for row in mapped.chunks(stride as usize) {
        pixels.extend_from_slice(&row[..(width * 4) as usize]);
    }
    drop(mapped);
    buffer.unmap();
    Ok(pixels)
}

pub fn write_png(path: &Path, width: u32, height: u32, pixels: &[u8]) -> Result<()> {
    let mut encoder = png::Encoder::new(BufWriter::new(File::create(path)?), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(pixels)?;
    Ok(())
}

pub fn read_png(path: &Path) -> Result<(u32, u32, Vec<u8>)> {
    let mut reader = png::Decoder::new(BufReader::new(File::open(path)?)).read_info()?;
    let mut pixels = vec![
        0;
        reader
            .output_buffer_size()
            .context("PNG exceeds decoder limits")?
    ];
    let info = reader.next_frame(&mut pixels)?;
    anyhow::ensure!(
        info.color_type == png::ColorType::Rgba && info.bit_depth == png::BitDepth::Eight,
        "Expected RGBA8 snapshot"
    );
    pixels.truncate(info.buffer_size());
    Ok((info.width, info.height, pixels))
}
