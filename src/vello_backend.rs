//! Native wgpu surface used by the Vello desktop compositor.

use std::{num::NonZeroUsize, sync::Arc};

use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene};
use wgpu::{
    CurrentSurfaceTexture, Device, Queue, Surface, SurfaceConfiguration, Texture, TextureFormat,
    TextureView, util::TextureBlitter,
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
    scene_texture: Option<Texture>,
    scene_view: Option<TextureView>,
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
            label: Some("Acorn-2026 Vello device"),
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

        let mut result = Self {
            _instance: instance,
            surface,
            device,
            queue,
            config,
            renderer,
            blitter,
            scene_texture: None,
            scene_view: None,
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
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Acorn-2026 Vello scene target"),
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
            self.scene_texture = Some(texture);
            self.scene_view = Some(view);
        }
    }

    pub(crate) fn render(&mut self, scene: &Scene) -> Result<(), String> {
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
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Acorn-2026 Vello present encoder"),
            });
        // If the platform only supplies an sRGB surface, decode on sampling
        // so the destination encoding preserves Vello's original colour values.
        let srgb_view = self.config.format.is_srgb().then(|| {
            self.scene_texture
                .as_ref()
                .unwrap()
                .create_view(&wgpu::TextureViewDescriptor {
                    format: Some(TextureFormat::Rgba8UnormSrgb),
                    usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
                    ..Default::default()
                })
        });
        self.blitter.copy(
            &self.device,
            &mut encoder,
            srgb_view.as_ref().unwrap_or(view),
            &output_view,
        );
        self.queue.submit([encoder.finish()]);
        output.present();
        Ok(())
    }
}

/// Render the real GPU scene for visual verification, independent of a host window.
pub(crate) fn snapshot_scene(scene: &Scene, width: u32, height: u32) -> Result<Vec<u8>, String> {
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
    // Exercise the sRGB presentation fallback as well as scene rendering.
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
        format: TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let sampled = texture.create_view(&wgpu::TextureViewDescriptor {
        format: Some(TextureFormat::Rgba8UnormSrgb),
        usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
        ..Default::default()
    });
    TextureBlitter::new(&device, TextureFormat::Rgba8UnormSrgb).copy(
        &device,
        &mut encoder,
        &sampled,
        &presented.create_view(&Default::default()),
    );
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
