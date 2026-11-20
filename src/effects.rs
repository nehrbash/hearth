use std::io::{BufReader, Cursor};

use anyhow::{Context, Result, ensure};
use wgpu::util::DeviceExt;

use crate::{
    gpu::Gpu,
    render::Renderer,
    scene::{Scene, rect},
};

pub const LAYER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

pub struct Layer {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub size: [u32; 2],
}

impl Layer {
    pub fn new(gpu: &Gpu, size: [u32; 2]) -> Result<Self> {
        let maximum = gpu.device.limits().max_texture_dimension_2d;
        ensure!(
            size.iter().all(|value| *value > 0 && *value <= maximum),
            "Invalid effect layer size"
        );
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Effect layer"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: LAYER_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Ok(Self {
            texture,
            view,
            size,
        })
    }

    pub fn from_png(gpu: &Gpu, bytes: &[u8]) -> Result<Self> {
        let mut reader = png::Decoder::new(BufReader::new(Cursor::new(bytes))).read_info()?;
        let mut pixels = vec![
            0;
            reader
                .output_buffer_size()
                .context("Image exceeds decoder limits")?
        ];
        let info = reader.next_frame(&mut pixels)?;
        ensure!(
            info.color_type == png::ColorType::Rgba && info.bit_depth == png::BitDepth::Eight,
            "Effect image must be RGBA8"
        );
        let layer = Self::new(gpu, [info.width, info.height])?;
        gpu.queue.write_texture(
            layer.texture.as_image_copy(),
            &pixels[..info.buffer_size()],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(info.width * 4),
                rows_per_image: Some(info.height),
            },
            layer.texture.size(),
        );
        Ok(layer)
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ShaderUniforms {
    pub viewport: [f32; 4],
    pub parameters: [f32; 4],
}

const HEADER: &str = r#"
struct Uniforms {
    viewport: vec4<f32>,
    parameters: vec4<f32>,
}
@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var source_sampler: sampler;
struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}
@vertex
fn vertex(@builtin(vertex_index) index: u32) -> VertexOut {
    let uv = vec2(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VertexOut;
    out.position = vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}
"#;

pub struct Shader {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    sampler: wgpu::Sampler,
}

impl Shader {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat, fragment: &str) -> Result<Self> {
        Self::pipeline(gpu, format, fragment, None)
    }

    fn pipeline(
        gpu: &Gpu,
        format: wgpu::TextureFormat,
        fragment: &str,
        blend: Option<wgpu::BlendState>,
    ) -> Result<Self> {
        let errors = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Custom effect"),
                source: wgpu::ShaderSource::Wgsl(format!("{HEADER}\n{fragment}").into()),
            });
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Effect inputs"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Effect layout"),
                bind_group_layouts: &[&layout],
                immediate_size: 0,
            });
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Effect shader"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fragment"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            });
        if let Some(error) = pollster::block_on(errors.pop()) {
            anyhow::bail!("Invalid effect shader: {error}");
        }
        let uniforms = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Effect uniforms"),
                contents: bytemuck::bytes_of(&ShaderUniforms {
                    viewport: [0.0; 4],
                    parameters: [0.0; 4],
                }),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Effect sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Ok(Self {
            pipeline,
            layout,
            uniforms,
            sampler,
        })
    }

    pub fn render(
        &self,
        gpu: &Gpu,
        target: &wgpu::TextureView,
        source: &wgpu::TextureView,
        uniforms: ShaderUniforms,
    ) -> Result<()> {
        self.pass(gpu, target, source, uniforms, None)
    }

    fn pass(
        &self,
        gpu: &Gpu,
        target: &wgpu::TextureView,
        source: &wgpu::TextureView,
        uniforms: ShaderUniforms,
        clip: Option<crate::scene::Bounds>,
    ) -> Result<()> {
        ensure!(
            uniforms
                .viewport
                .iter()
                .chain(&uniforms.parameters)
                .all(|value| value.is_finite()),
            "Effect uniforms must be finite"
        );
        ensure!(
            uniforms.viewport[0] > 0.0 && uniforms.viewport[1] > 0.0,
            "Effect dimensions must be positive"
        );
        gpu.queue
            .write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));
        let inputs = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Effect sources"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Effect pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: if clip.is_some() {
                            wgpu::LoadOp::Load
                        } else {
                            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                        },
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(clip) = clip {
                let x = clip.x.ceil() as u32;
                let y = clip.y.ceil() as u32;
                let width = ((clip.x + clip.width).floor() as u32).saturating_sub(x);
                let height = ((clip.y + clip.height).floor() as u32).saturating_sub(y);
                if width == 0 || height == 0 {
                    return Ok(());
                }
                pass.set_scissor_rect(x, y, width, height);
            }
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &inputs, &[]);
            pass.draw(0..3, 0..1);
        }
        gpu.queue.submit([encoder.finish()]);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Mode {
    Moon,
    Layer,
    Blur,
    Joined,
    Separated,
}

pub struct EffectRenderer {
    moon: Shader,
    joined: Shader,
    horizontal: Shader,
    vertical: Shader,
    art: Layer,
    layers: Option<(Layer, Layer)>,
    geometry: Renderer,
}

impl EffectRenderer {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Result<Self> {
        Ok(Self {
            moon: Shader::new(gpu, format, include_str!("../shaders/moon.wgsl"))?,
            joined: Shader::new(gpu, format, include_str!("../shaders/joined.wgsl"))?,
            horizontal: Shader::new(gpu, LAYER_FORMAT, include_str!("../shaders/blur.wgsl"))?,
            vertical: Shader::new(gpu, format, include_str!("../shaders/blur.wgsl"))?,
            art: Layer::from_png(gpu, include_bytes!("../fixtures/effects/crimson-moon.png"))?,
            layers: None,
            geometry: Renderer::new(gpu, LAYER_FORMAT),
        })
    }

    pub fn render(
        &mut self,
        gpu: &Gpu,
        view: &wgpu::TextureView,
        size: [u32; 2],
        mode: Mode,
        time: f32,
    ) -> Result<()> {
        ensure!(
            time.is_finite() && time >= 0.0,
            "Effect time must be finite and nonnegative"
        );
        let mut uniforms = ShaderUniforms {
            viewport: [size[0] as f32, size[1] as f32, time, (time / 8.0).fract()],
            parameters: [0.0, 0.0, 2.5, 40.0],
        };
        match mode {
            Mode::Moon => self.moon.render(gpu, view, &self.art.view, uniforms),
            Mode::Joined | Mode::Separated => {
                if matches!(mode, Mode::Separated) {
                    uniforms.parameters[3] = 0.0;
                }
                self.joined.render(gpu, view, &self.art.view, uniforms)
            }
            Mode::Layer | Mode::Blur => {
                if self
                    .layers
                    .as_ref()
                    .is_none_or(|(layer, _)| layer.size != size)
                {
                    self.layers = Some((Layer::new(gpu, size)?, Layer::new(gpu, size)?));
                }
                let (source, temporary) = self.layers.as_ref().context("Missing effect layers")?;
                self.geometry
                    .draw(gpu, &source.view, &layer_scene(size)?, size)?;
                uniforms.parameters[0] = 1.0;
                if matches!(mode, Mode::Layer) {
                    uniforms.parameters[2] = 0.0;
                    self.vertical.render(gpu, view, &source.view, uniforms)
                } else {
                    self.horizontal
                        .render(gpu, &temporary.view, &source.view, uniforms)?;
                    uniforms.parameters[0] = 0.0;
                    uniforms.parameters[1] = 1.0;
                    self.vertical.render(gpu, view, &temporary.view, uniforms)
                }
            }
        }
    }
}

fn layer_scene(size: [u32; 2]) -> Result<Scene> {
    let w = size[0] as f32;
    let h = size[1] as f32;
    Scene::new(vec![
        rect(
            w * 0.18,
            h * 0.18,
            w * 0.64,
            h * 0.64,
            [0.10, 0.16, 0.24, 1.0],
        ),
        rect(w * 0.25, h * 0.3, w * 0.2, h * 0.4, [0.15, 0.65, 0.48, 1.0]),
        rect(w * 0.52, h * 0.3, w * 0.2, h * 0.1, [0.72, 0.38, 0.16, 1.0]),
        rect(
            w * 0.52,
            h * 0.48,
            w * 0.2,
            h * 0.04,
            [0.55, 0.63, 0.76, 1.0],
        ),
        rect(
            w * 0.52,
            h * 0.59,
            w * 0.14,
            h * 0.04,
            [0.30, 0.38, 0.52, 1.0],
        ),
    ])
}

const COMPOSITE: &str = r#"
@fragment
fn fragment(in: VertexOut) -> @location(0) vec4<f32> {
    let uv = (in.uv * uniforms.viewport.xy - uniforms.parameters.xy) / uniforms.parameters.zw;
    if any(uv < vec2(0.0)) || any(uv > vec2(1.0)) { discard; }
    return textureSampleLevel(source, source_sampler, uv, 0.0);
}
"#;

pub(crate) struct NodeSurface {
    pub(crate) source: Layer,
    temporary: Layer,
    output: Layer,
    shader: Option<Shader>,
    art: std::sync::Arc<std::sync::OnceLock<Layer>>,
    child: Option<Box<Renderer>>,
    horizontal: Shader,
    composite: Shader,
    pub(crate) key: Option<crate::scene::ShaderKind>,
}

impl NodeSurface {
    pub(crate) fn new(
        gpu: &Gpu,
        format: wgpu::TextureFormat,
        size: [u32; 2],
        key: Option<crate::scene::ShaderKind>,
        art: std::sync::Arc<std::sync::OnceLock<Layer>>,
    ) -> Result<Self> {
        use crate::scene::ShaderKind;
        let fragment = match &key {
            Some(ShaderKind::Moon) => Some(include_str!("../shaders/moon.wgsl")),
            Some(ShaderKind::Joined) => Some(include_str!("../shaders/joined.wgsl")),
            Some(ShaderKind::Custom(fragment)) => Some(fragment.as_str()),
            None => None,
        };
        let shader = fragment
            .map(|fragment| Shader::new(gpu, LAYER_FORMAT, fragment))
            .transpose()?;
        if matches!(key, Some(ShaderKind::Moon)) && art.get().is_none() {
            let _ = art.set(Layer::from_png(
                gpu,
                include_bytes!("../fixtures/effects/crimson-moon.png"),
            )?);
        }
        let child = if key.is_none() {
            let mut renderer = Renderer::new(gpu, LAYER_FORMAT);
            renderer.moon_art = art.clone();
            Some(Box::new(renderer))
        } else {
            None
        };
        Ok(Self {
            source: Layer::new(gpu, size)?,
            temporary: Layer::new(gpu, size)?,
            output: Layer::new(gpu, size)?,
            shader,
            art,
            child,
            horizontal: Shader::new(gpu, LAYER_FORMAT, include_str!("../shaders/blur.wgsl"))?,
            composite: Shader::pipeline(
                gpu,
                format,
                COMPOSITE,
                Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            )?,
            key,
        })
    }

    pub(crate) fn prepare(&mut self, gpu: &Gpu, node: &crate::scene::Node) -> Result<()> {
        if let crate::scene::Node::Layer { children, .. } = node {
            let scene = Scene::new(children.clone())?;
            self.child
                .as_mut()
                .context("Missing layer renderer")?
                .prepare(gpu, &scene, self.source.size)?;
        }
        Ok(())
    }

    pub(crate) fn draw(
        &mut self,
        gpu: &Gpu,
        target: &wgpu::TextureView,
        size: [u32; 2],
        bounds: crate::scene::Bounds,
        clip: crate::scene::Bounds,
        node: &crate::scene::Node,
    ) -> Result<()> {
        use crate::scene::{LayerEffect, Node};
        let mut uniforms = ShaderUniforms {
            viewport: [
                self.source.size[0] as f32,
                self.source.size[1] as f32,
                0.0,
                0.0,
            ],
            parameters: [0.0; 4],
        };
        let output = match node {
            Node::Shader {
                time, parameters, ..
            } => {
                uniforms.viewport[2] = *time;
                uniforms.viewport[3] = (*time / 8.0).fract();
                uniforms.parameters = *parameters;
                self.shader.as_ref().context("Missing shader")?.render(
                    gpu,
                    &self.source.view,
                    if matches!(self.key, Some(crate::scene::ShaderKind::Moon)) {
                        &self.art.get().context("Missing moon texture")?.view
                    } else {
                        &self.temporary.view
                    },
                    uniforms,
                )?;
                &self.source.view
            }
            Node::Layer {
                children, effect, ..
            } => {
                self.child
                    .as_mut()
                    .context("Missing layer renderer")?
                    .draw_with_clear(
                        gpu,
                        &self.source.view,
                        &Scene::new(children.clone())?,
                        self.source.size,
                        wgpu::Color::TRANSPARENT,
                    )?;
                if let LayerEffect::Blur { radius } = effect {
                    uniforms.parameters = [1.0, 0.0, *radius, 0.0];
                    self.horizontal.render(
                        gpu,
                        &self.temporary.view,
                        &self.source.view,
                        uniforms,
                    )?;
                    uniforms.parameters[0] = 0.0;
                    uniforms.parameters[1] = 1.0;
                    self.horizontal.render(
                        gpu,
                        &self.output.view,
                        &self.temporary.view,
                        uniforms,
                    )?;
                    &self.output.view
                } else {
                    &self.source.view
                }
            }
            _ => anyhow::bail!("Expected a shader or layer node"),
        };
        self.composite.pass(
            gpu,
            target,
            output,
            ShaderUniforms {
                viewport: [size[0] as f32, size[1] as f32, 0.0, 0.0],
                parameters: [bounds.x, bounds.y, bounds.width, bounds.height],
            },
            Some(clip),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn pixel(pixels: &[u8], x: usize, y: usize) -> &[u8] {
        &pixels[(y * 640 + x) * 4..(y * 640 + x) * 4 + 4]
    }

    fn frame(
        gpu: &Gpu,
        effects: &mut EffectRenderer,
        layer: &Layer,
        mode: Mode,
        time: f32,
    ) -> Result<Vec<u8>> {
        effects.render(gpu, &layer.view, layer.size, mode, time)?;
        crate::render::read_texture(gpu, &layer.texture, layer.size[0], layer.size[1])
    }

    #[test]
    fn invalid_nested_shader_preserves_the_rendered_target() -> Result<()> {
        use crate::scene::{Bounds, LayerEffect, Node, ShaderKind};
        let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
        let target = Layer::new(&gpu, [64, 64])?;
        let mut renderer = Renderer::new(&gpu, LAYER_FORMAT);
        renderer.draw(
            &gpu,
            &target.view,
            &Scene::new(vec![rect(0.0, 0.0, 64.0, 64.0, [0.4, 0.2, 0.1, 1.0])])?,
            target.size,
        )?;
        let before = crate::render::read_texture(&gpu, &target.texture, 64, 64)?;
        let invalid = Scene::new(vec![Node::Layer {
            bounds: Bounds::new(0.0, 0.0, 32.0, 32.0),
            effect: LayerEffect::None,
            children: vec![Node::Shader {
                bounds: Bounds::new(0.0, 0.0, 16.0, 16.0),
                kind: ShaderKind::Custom("invalid shader".into()),
                time: 0.0,
                parameters: [0.0; 4],
            }],
        }])?;
        assert!(
            renderer
                .draw(&gpu, &target.view, &invalid, target.size)
                .is_err()
        );
        assert_eq!(
            before,
            crate::render::read_texture(&gpu, &target.texture, 64, 64)?
        );
        renderer.draw(&gpu, &target.view, &Scene::new(vec![])?, target.size)?;
        assert_ne!(
            before,
            crate::render::read_texture(&gpu, &target.texture, 64, 64)?
        );
        Ok(())
    }

    #[test]
    fn gpu_effects_match_snapshots_and_change_the_intended_pixels() -> Result<()> {
        let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
        let mut effects = EffectRenderer::new(&gpu, LAYER_FORMAT)?;
        let layer = Layer::new(&gpu, [640, 400])?;
        let mut rendered = Vec::new();
        for (name, mode, time) in [
            ("moon-new", Mode::Moon, 0.0),
            ("moon-quarter", Mode::Moon, 2.0),
            ("moon-full", Mode::Moon, 4.0),
            ("layer", Mode::Layer, 0.0),
            ("blur", Mode::Blur, 0.0),
            ("joined", Mode::Joined, 0.0),
            ("separated", Mode::Separated, 0.0),
        ] {
            let actual = frame(&gpu, &mut effects, &layer, mode, time)?;
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/effects/snapshots")
                .join(format!("{name}.png"));
            let (width, height, expected) = crate::render::read_png(&path)?;
            assert_eq!((width, height), (640, 400));
            assert!(actual == expected, "Effect differs from {}", path.display());
            rendered.push(actual);
        }
        let brightness = |image: &[u8], left: usize, right: usize| -> u64 {
            (180..220)
                .flat_map(|y| (left..right).map(move |x| u64::from(pixel(image, x, y)[0])))
                .sum()
        };
        assert!(brightness(&rendered[2], 240, 400) > brightness(&rendered[0], 240, 400) * 2);
        assert!(
            brightness(&rendered[1], 350, 400) > brightness(&rendered[1], 240, 290),
            "quarter moon brightness left={} right={}",
            brightness(&rendered[1], 240, 290),
            brightness(&rendered[1], 350, 400)
        );
        let crisp_outside = pixel(&rendered[3], 156, 200)[1];
        let blurred_outside = pixel(&rendered[4], 156, 200)[1];
        let crisp_inside = pixel(&rendered[3], 164, 200)[1];
        assert!(crisp_outside < blurred_outside && blurred_outside < crisp_inside);
        assert_eq!(pixel(&rendered[6], 320, 200), pixel(&rendered[6], 20, 20));
        assert_ne!(pixel(&rendered[5], 320, 200), pixel(&rendered[5], 20, 20));
        Ok(())
    }

    #[test]
    fn shader_validation_is_recoverable_and_layers_reject_empty_sizes() -> Result<()> {
        let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
        assert!(Layer::new(&gpu, [0, 32]).is_err());
        assert!(Shader::new(&gpu, LAYER_FORMAT, "this is not WGSL").is_err());
        let mut effects = EffectRenderer::new(&gpu, LAYER_FORMAT)?;
        let layer = Layer::new(&gpu, [640, 400])?;
        assert!(
            effects
                .render(&gpu, &layer.view, layer.size, Mode::Moon, f32::NAN)
                .is_err()
        );
        assert!(
            effects
                .render(&gpu, &layer.view, layer.size, Mode::Moon, -1.0)
                .is_err()
        );
        effects.render(&gpu, &layer.view, layer.size, Mode::Joined, 0.0)?;
        Ok(())
    }
}
