//! GPU rendering context — the wgpu surface/device/queue plumbing.
//!
//! Spec §3.2 (rendering strategy). This crate owns the wgpu objects and the
//! surface lifecycle; orchestrating individual render passes is left to the
//! caller until the scene graph lands (M1, later PR).

use std::sync::Arc;

use wgpu::{
    CompositeAlphaMode, CurrentSurfaceTexture, DeviceDescriptor, Instance, PresentMode,
    RequestAdapterOptions, SurfaceConfiguration, TextureFormat, TextureUsages,
};
use winit::window::Window;

/// Owns the wgpu device, queue, and surface for a single window.
pub struct GpuContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface_config: SurfaceConfiguration,
    surface: wgpu::Surface<'static>,
}

impl GpuContext {
    /// Create a context bound to `window`. Blocks on adapter/device requests —
    /// acceptable at startup; not on a hot path.
    pub fn new(window: Arc<Window>) -> Self {
        let size = window.inner_size();

        let instance = Instance::default();
        let surface = instance
            .create_surface(window)
            .expect("failed to create wgpu surface");

        let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .expect("no compatible wgpu adapter found");

        let (device, queue) =
            pollster::block_on(adapter.request_device(&DeviceDescriptor::default()))
                .expect("failed to request device");

        let surface_config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format: TextureFormat::Bgra8UnormSrgb,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: PresentMode::Fifo,
            alpha_mode: CompositeAlphaMode::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        Self {
            device,
            queue,
            surface_config,
            surface,
        }
    }

    /// Surface texture format — the format render pipelines must target.
    pub fn format(&self) -> TextureFormat {
        self.surface_config.format
    }

    /// Reconfigure the surface for a new window size. A zero dimension is
    /// ignored (the window is minimized).
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
    }

    /// Acquire the next surface texture. Returns `None` when the frame should
    /// be skipped; reconfigures the surface internally on lost/outdated.
    pub fn acquire(&mut self) -> Option<wgpu::SurfaceTexture> {
        match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(frame) | CurrentSurfaceTexture::Suboptimal(frame) => {
                Some(frame)
            }
            CurrentSurfaceTexture::Outdated | CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.surface_config);
                None
            }
            CurrentSurfaceTexture::Timeout
            | CurrentSurfaceTexture::Occluded
            | CurrentSurfaceTexture::Validation => None,
        }
    }
}
