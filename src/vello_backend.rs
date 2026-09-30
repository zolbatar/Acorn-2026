//! Native wgpu surface used by the Vello desktop compositor.

use std::{
    num::{NonZeroU64, NonZeroUsize},
    sync::{Arc, OnceLock},
};

use crate::display::DisplayColour;
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene};
use wgpu::{
    BindGroup, BindGroupLayout, Buffer, CurrentSurfaceTexture, Device, Queue, RenderPipeline,
    Surface, SurfaceConfiguration, Texture, TextureFormat, TextureView, util::TextureBlitter,
};
use winit::window::Window;

pub(crate) struct VelloSurface {
    _instance: wgpu::Instance,
    surface: Surface<'static>,
    device: Device,
    queue: Queue,
    config: SurfaceConfiguration,
    renderer: Renderer,
    blitter: TextureBlitter,
    output_processor: OutputProcessor,
    scene_texture: Option<Texture>,
    scene_view: Option<TextureView>,
    scene_srgb_view: Option<TextureView>,
}

struct PaletteLookup {
    _texture: Texture,
    view: TextureView,
}

struct OutputProcessor {
    layout: BindGroupLayout,
    pipeline: RenderPipeline,
    parameters: Buffer,
    fallback_palette: PaletteLookup,
    palette_16: Option<PaletteLookup>,
    palette_256: Option<PaletteLookup>,
    target_is_srgb: bool,
    bound_group: Option<BindGroup>,
    bound_colour: Option<DisplayColour>,
}

impl OutputProcessor {
    fn new(device: &Device, queue: &Queue, target_format: TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Ricochet display output bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(16),
                    },
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Ricochet display output shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("display_output.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Ricochet display output pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Ricochet display output pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let parameters = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Ricochet display output parameters"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let fallback_palette = create_palette_lookup(device, queue, &[0, 0, 0, 255], 1);
        Self {
            layout,
            pipeline,
            parameters,
            fallback_palette,
            palette_16: None,
            palette_256: None,
            target_is_srgb: target_format.is_srgb(),
            bound_group: None,
            bound_colour: None,
        }
    }

    fn invalidate_binding(&mut self) {
        self.bound_group = None;
        self.bound_colour = None;
    }

    fn ensure_palette(
        &mut self,
        device: &Device,
        queue: &Queue,
        colour: DisplayColour,
    ) -> Result<(), String> {
        match colour {
            DisplayColour::Colour16 if self.palette_16.is_none() => {
                self.palette_16 = Some(create_palette_lookup(
                    device,
                    queue,
                    palette_lookup_16(),
                    32,
                ));
            }
            DisplayColour::Colour256 if self.palette_256.is_none() => {
                self.palette_256 = Some(create_palette_lookup(
                    device,
                    queue,
                    palette_lookup_256(),
                    32,
                ));
            }
            _ => {}
        }
        Ok(())
    }

    fn render(
        &mut self,
        device: &Device,
        queue: &Queue,
        source: &TextureView,
        target: &TextureView,
        colour: DisplayColour,
    ) -> Result<(), String> {
        self.ensure_palette(device, queue, colour)?;
        if self.bound_colour != Some(colour) || self.bound_group.is_none() {
            let palette = match colour {
                DisplayColour::Colour16 => &self.palette_16.as_ref().unwrap().view,
                DisplayColour::Colour256 => &self.palette_256.as_ref().unwrap().view,
                _ => &self.fallback_palette.view,
            };
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Ricochet display output bind group"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(palette),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.parameters.as_entire_binding(),
                    },
                ],
            });
            self.bound_group = Some(bind_group);
            self.bound_colour = Some(colour);
        }

        let mut parameters = [0_u8; 16];
        parameters[..4].copy_from_slice(&colour.id().to_le_bytes());
        parameters[4..8].copy_from_slice(&u32::from(self.target_is_srgb).to_le_bytes());
        queue.write_buffer(&self.parameters, 0, &parameters);

        let mut pass = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Ricochet display output encoder"),
        });
        {
            let mut render_pass = pass.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Ricochet display output pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
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
            render_pass.set_pipeline(&self.pipeline);
            render_pass.set_bind_group(0, self.bound_group.as_ref().unwrap(), &[]);
            render_pass.draw(0..3, 0..1);
        }
        queue.submit([pass.finish()]);
        Ok(())
    }
}

fn create_palette_lookup(
    device: &Device,
    queue: &Queue,
    rgba: &[u8],
    dimension: u32,
) -> PaletteLookup {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Ricochet desktop colour lookup"),
        size: wgpu::Extent3d {
            width: dimension,
            height: dimension,
            depth_or_array_layers: dimension,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(dimension * 4),
            rows_per_image: Some(dimension),
        },
        wgpu::Extent3d {
            width: dimension,
            height: dimension,
            depth_or_array_layers: dimension,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    PaletteLookup {
        _texture: texture,
        view,
    }
}

fn palette_lookup_16() -> &'static [u8] {
    static LOOKUP: OnceLock<Vec<u8>> = OnceLock::new();
    LOOKUP
        .get_or_init(|| build_palette_lookup(crate::riscos_resources::default_palette(4)))
        .as_slice()
}

fn palette_lookup_256() -> &'static [u8] {
    static LOOKUP: OnceLock<Vec<u8>> = OnceLock::new();
    LOOKUP
        .get_or_init(|| build_palette_lookup(crate::riscos_resources::default_palette(8)))
        .as_slice()
}

fn build_palette_lookup(
    palette: Result<Vec<[u8; 4]>, crate::riscos_resources::ResourceError>,
) -> Vec<u8> {
    let palette = palette.expect("bundled RISC OS desktop palettes are valid");
    let linear_palette = palette
        .iter()
        .map(|rgba| {
            rgba[..3]
                .iter()
                .map(|channel| srgb_to_linear(*channel))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut rgba = Vec::with_capacity(32 * 32 * 32 * 4);
    for blue in 0..32 {
        for green in 0..32 {
            for red in 0..32 {
                let source = [red, green, blue]
                    .map(|channel| srgb_to_linear((channel * 8 + 4).min(255) as u8));
                let mut nearest = 0;
                let mut nearest_distance = f32::INFINITY;
                for (index, candidate) in linear_palette.iter().enumerate() {
                    let distance = (source[0] - candidate[0]).powi(2)
                        + (source[1] - candidate[1]).powi(2)
                        + (source[2] - candidate[2]).powi(2);
                    if distance < nearest_distance {
                        nearest = index;
                        nearest_distance = distance;
                    }
                }
                rgba.extend_from_slice(&palette[nearest]);
            }
        }
    }
    rgba
}

fn srgb_to_linear(channel: u8) -> f32 {
    let channel = f32::from(channel) / 255.0;
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

impl VelloSurface {
    pub(crate) fn new(window: Arc<Window>) -> Result<Self, String> {
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window.clone())
            .map_err(|error| format!("could not create wgpu surface: {error}"))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .map_err(|error| format!("could not find a compute-capable graphics adapter: {error}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("Ricochet Vello device"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            memory_hints: wgpu::MemoryHints::Performance,
            ..Default::default()
        }))
        .map_err(|error| format!("could not create wgpu device: {error}"))?;

        let capabilities = surface.get_capabilities(&adapter);
        // Vello writes display-encoded colours. Prefer a non-sRGB target to
        // avoid applying the sRGB transfer function a second time at presentation.
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| !format.is_srgb())
            .or_else(|| capabilities.formats.first().copied())
            .ok_or_else(|| "the graphics surface reports no supported formats".to_owned())?;
        let alpha_mode = capabilities
            .alpha_modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Auto);
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| "the graphics adapter cannot configure the window surface".to_owned())?;
        config.format = format;
        config.alpha_mode = alpha_mode;
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = 1;
        surface.configure(&device, &config);

        let renderer = Renderer::new(
            &device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::area_only(),
                num_init_threads: NonZeroUsize::new(1),
                pipeline_cache: None,
            },
        )
        .map_err(|error| format!("could not initialize the Vello GPU renderer: {error}"))?;
        let blitter = TextureBlitter::new(&device, format);
        let output_processor = OutputProcessor::new(&device, &queue, format);

        let mut result = Self {
            _instance: instance,
            surface,
            device,
            queue,
            config,
            renderer,
            blitter,
            output_processor,
            scene_texture: None,
            scene_view: None,
            scene_srgb_view: None,
        };
        result.resize(size.width, size.height);
        Ok(result)
    }

    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if self.config.width != width || self.config.height != height {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
        }
        let matches = self.scene_texture.as_ref().is_some_and(|texture| {
            let size = texture.size();
            size.width == width && size.height == height
        });
        if !matches {
            self.output_processor.invalidate_binding();
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Ricochet Vello scene target"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[TextureFormat::Rgba8UnormSrgb],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let srgb_view = texture.create_view(&wgpu::TextureViewDescriptor {
                format: Some(TextureFormat::Rgba8UnormSrgb),
                usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
                ..Default::default()
            });
            self.scene_texture = Some(texture);
            self.scene_view = Some(view);
            self.scene_srgb_view = Some(srgb_view);
        }
    }

    pub(crate) fn render(&mut self, scene: &Scene, colour: DisplayColour) -> Result<(), String> {
        let width = self.config.width;
        let height = self.config.height;
        if width == 0 || height == 0 {
            return Ok(());
        }
        self.resize(width, height);
        let view = self
            .scene_view
            .as_ref()
            .expect("surface resize creates a scene texture");
        self.renderer
            .render_to_texture(
                &self.device,
                &self.queue,
                scene,
                view,
                &RenderParams {
                    base_color: vello::peniko::Color::from_rgb8(78, 78, 78),
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .map_err(|error| format!("Vello failed to render the desktop scene: {error}"))?;

        let mut retried_surface = false;
        let output = loop {
            match self.surface.get_current_texture() {
                CurrentSurfaceTexture::Success(output) => break output,
                CurrentSurfaceTexture::Suboptimal(output) => {
                    self.surface.configure(&self.device, &self.config);
                    break output;
                }
                CurrentSurfaceTexture::Lost | CurrentSurfaceTexture::Outdated
                    if !retried_surface =>
                {
                    self.surface.configure(&self.device, &self.config);
                    retried_surface = true;
                }
                CurrentSurfaceTexture::Lost | CurrentSurfaceTexture::Outdated => return Ok(()),
                CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => return Ok(()),
                CurrentSurfaceTexture::Validation => {
                    return Err("the graphics surface rejected the frame".to_owned());
                }
            }
        };
        let output_view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        if colour == DisplayColour::Rgb888 {
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Ricochet Vello present encoder"),
                });
            let source = if self.config.format.is_srgb() {
                self.scene_srgb_view.as_ref().unwrap()
            } else {
                view
            };
            self.blitter
                .copy(&self.device, &mut encoder, source, &output_view);
            self.queue.submit([encoder.finish()]);
        } else {
            let source = if self.config.format.is_srgb() {
                self.scene_srgb_view.as_ref().unwrap()
            } else {
                view
            };
            self.output_processor.render(
                &self.device,
                &self.queue,
                source,
                &output_view,
                colour,
            )?;
        }
        output.present();
        Ok(())
    }
}

/// Render the real GPU scene for visual verification, independent of a host window.
pub(crate) fn snapshot_scene(scene: &Scene, width: u32, height: u32) -> Result<Vec<u8>, String> {
    snapshot_scene_with_colour(scene, width, height, DisplayColour::Rgb888)
}

/// Render and read back a GPU snapshot after the selected output colour transform.
pub(crate) fn snapshot_scene_with_colour(
    scene: &Scene,
    width: u32,
    height: u32,
    colour: DisplayColour,
) -> Result<Vec<u8>, String> {
    snapshot_scene_with_colour_format(scene, width, height, colour, TextureFormat::Rgba8UnormSrgb)
}

fn snapshot_scene_with_colour_format(
    scene: &Scene,
    width: u32,
    height: u32,
    colour: DisplayColour,
    presentation_format: TextureFormat,
) -> Result<Vec<u8>, String> {
    let instance = wgpu::Instance::default();
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .map_err(|e| e.to_string())?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: adapter.limits(),
        ..Default::default()
    }))
    .map_err(|e| e.to_string())?;
    let mut renderer = Renderer::new(
        &device,
        RendererOptions {
            use_cpu: false,
            antialiasing_support: AaSupport::area_only(),
            num_init_threads: NonZeroUsize::new(1),
            pipeline_cache: None,
        },
    )
    .map_err(|e| e.to_string())?;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Vello snapshot"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[TextureFormat::Rgba8UnormSrgb],
    });
    renderer
        .render_to_texture(
            &device,
            &queue,
            scene,
            &texture.create_view(&Default::default()),
            &RenderParams {
                base_color: vello::peniko::Color::BLACK,
                width,
                height,
                antialiasing_method: AaConfig::Area,
            },
        )
        .map_err(|e| e.to_string())?;
    let stride = (width * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Vello snapshot readback"),
        size: u64::from(stride) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    // Exercise both the preferred linear UNORM surface and the sRGB fallback.
    let presented = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Vello snapshot presentation"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: presentation_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let sampled = if presentation_format.is_srgb() {
        texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(TextureFormat::Rgba8UnormSrgb),
            usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
            ..Default::default()
        })
    } else {
        texture.create_view(&wgpu::TextureViewDescriptor {
            usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
            ..Default::default()
        })
    };
    let presented_view = presented.create_view(&Default::default());
    if colour == DisplayColour::Rgb888 {
        TextureBlitter::new(&device, presentation_format).copy(
            &device,
            &mut encoder,
            &sampled,
            &presented_view,
        );
    } else {
        let mut output_processor = OutputProcessor::new(&device, &queue, presentation_format);
        output_processor.render(&device, &queue, &sampled, &presented_view, colour)?;
    }
    encoder.copy_texture_to_buffer(
        presented.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| e.to_string())?;
    rx.recv()
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let mapped = buffer.slice(..).get_mapped_range();
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for row in mapped.chunks_exact(stride as usize) {
        rgba.extend_from_slice(&row[..(width * 4) as usize]);
    }
    drop(mapped);
    buffer.unmap();
    Ok(rgba)
}

#[cfg(test)]
mod tests {
    use super::snapshot_scene_with_colour_format;
    use crate::{display::DisplayColour, riscos_resources::default_palette};
    use vello::{
        Scene,
        peniko::{
            Color, Fill,
            kurbo::{Affine, Rect},
        },
    };

    const WIDTH: u32 = 256;
    const HEIGHT: u32 = 32;
    const STRIPE_WIDTH: u32 = WIDTH / 8;
    const INPUT: [[u8; 3]; 8] = [
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
        [37, 91, 183],
        [255, 255, 255],
        [0, 0, 0],
        [128, 128, 128],
        [231, 173, 49],
    ];

    fn test_scene() -> Scene {
        let mut scene = Scene::new();
        for (index, [red, green, blue]) in INPUT.into_iter().enumerate() {
            let x = (index as u32 * STRIPE_WIDTH) as f64;
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgba8(red, green, blue, 255),
                None,
                &Rect::new(x, 0.0, x + f64::from(STRIPE_WIDTH), f64::from(HEIGHT)),
            );
        }
        scene
    }

    fn stripe_pixel(rgba: &[u8], stripe: usize) -> [u8; 4] {
        let x = stripe as u32 * STRIPE_WIDTH + STRIPE_WIDTH / 2;
        let y = HEIGHT / 2;
        let offset = ((y * WIDTH + x) * 4) as usize;
        rgba[offset..offset + 4].try_into().unwrap()
    }

    fn channel_in_levels(channel: u8, levels: u16) -> bool {
        let quantized = (f32::from(channel) * f32::from(levels - 1) / 255.0).round();
        let expanded = (quantized * 255.0 / f32::from(levels - 1)).round() as i16;
        (i16::from(channel) - expanded).abs() <= 1
    }

    fn render_profile(
        scene: &Scene,
        colour: DisplayColour,
        format: wgpu::TextureFormat,
    ) -> Vec<u8> {
        snapshot_scene_with_colour_format(scene, WIDTH, HEIGHT, colour, format)
            .unwrap_or_else(|error| panic!("the GPU renders {colour:?} to {format:?}: {error}"))
    }

    #[test]
    #[ignore = "requires a GPU; run with RICOCHET_VELLO_SNAPSHOT=1 and --ignored"]
    fn gpu_output_profiles_quantize_for_unorm_and_srgb_presentation() {
        assert!(
            std::env::var_os("RICOCHET_VELLO_SNAPSHOT")
                .or_else(|| std::env::var_os("ACORN_VELLO_SNAPSHOT"))
                .is_some(),
            "set RICOCHET_VELLO_SNAPSHOT=1 to opt into real GPU rendering"
        );

        let scene = test_scene();
        for format in [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        ] {
            let rgb888 = render_profile(&scene, DisplayColour::Rgb888, format);
            for (stripe, expected) in INPUT.iter().enumerate() {
                let pixel = stripe_pixel(&rgb888, stripe);
                for (actual, expected) in pixel[..3].iter().zip(expected) {
                    assert!(
                        (i16::from(*actual) - i16::from(*expected)).abs() <= 1,
                        "RGB888 {format:?} round trip changed stripe {stripe}: {pixel:?}"
                    );
                }
                assert_eq!(pixel[3], 255);
            }

            let grey4 = render_profile(&scene, DisplayColour::Grey4, format);
            let grey16 = render_profile(&scene, DisplayColour::Grey16, format);
            let grey256 = render_profile(&scene, DisplayColour::Grey256, format);
            let bw = render_profile(&scene, DisplayColour::BW, format);
            for (stripe, pixel) in (0..8).map(|stripe| (stripe, stripe_pixel(&grey4, stripe))) {
                assert_eq!(pixel[0], pixel[1]);
                assert_eq!(pixel[1], pixel[2]);
                assert!(
                    channel_in_levels(pixel[0], 4),
                    "4-grey {format:?} stripe {stripe}: {pixel:?}"
                );
                assert_eq!(pixel[3], 255);
            }
            for stripe in 0..8 {
                let pixel = stripe_pixel(&grey16, stripe);
                assert_eq!(pixel[0], pixel[1]);
                assert_eq!(pixel[1], pixel[2]);
                assert!(
                    channel_in_levels(pixel[0], 16),
                    "16-grey {format:?} stripe {stripe}: {pixel:?}"
                );
                assert_eq!(pixel[3], 255);

                let pixel = stripe_pixel(&grey256, stripe);
                assert_eq!(pixel[0], pixel[1]);
                assert_eq!(pixel[1], pixel[2]);
                assert_eq!(pixel[3], 255);

                let pixel = stripe_pixel(&bw, stripe);
                assert!(
                    pixel[0] == 0 || pixel[0] == 255,
                    "BW {format:?} stripe {stripe}: {pixel:?}"
                );
                assert_eq!(pixel[0], pixel[1]);
                assert_eq!(pixel[1], pixel[2]);
                assert_eq!(pixel[3], 255);
            }

            for (colour, palette_depth) in
                [(DisplayColour::Colour16, 4), (DisplayColour::Colour256, 8)]
            {
                let output = render_profile(&scene, colour, format);
                let palette =
                    default_palette(palette_depth).expect("bundled desktop palette is valid");
                for stripe in 0..8 {
                    let pixel = stripe_pixel(&output, stripe);
                    assert!(
                        palette.iter().any(|entry| entry[..3] == pixel[..3]),
                        "{colour:?} {format:?} stripe {stripe} is not in its palette: {pixel:?}"
                    );
                    assert_eq!(pixel[3], 255);
                }
            }

            let rgb555 = render_profile(&scene, DisplayColour::Rgb555, format);
            for stripe in 0..8 {
                let pixel = stripe_pixel(&rgb555, stripe);
                assert!(
                    pixel[..3]
                        .iter()
                        .all(|channel| channel_in_levels(*channel, 32)),
                    "RGB555 {format:?} stripe {stripe}: {pixel:?}"
                );
                assert_eq!(pixel[3], 255);
            }
        }
    }
}
