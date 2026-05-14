// Light Editor — application entry point.
//
// Currently runs the M0 spike behavior (see tasks/milestone-0-spike.md): one
// window rendering a multilingual sample, frame time logged every second. The
// GPU and text plumbing now live in editor-ui-render and editor-ui-text; this
// binary owns only the window lifecycle, the render-pass orchestration, and
// the latency instrumentation. Scene graph / retained-mode widgets are M1+.

use std::sync::Arc;
use std::time::Instant;

use editor_ui_render::GpuContext;
use editor_ui_text::glyphon::{Color, Resolution, TextArea, TextBounds};
use editor_ui_text::TextStack;
use wgpu::{
    LoadOp, Operations, RenderPassColorAttachment, RenderPassDescriptor, StoreOp,
    TextureViewDescriptor,
};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

const WINDOW_WIDTH: u32 = 1280;
const WINDOW_HEIGHT: u32 = 720;

/// Padding between the window edge and the text block, in physical pixels.
const TEXT_PADDING: f32 = 80.0;
const TEXT_INSET: f32 = 40.0;

// Per spec §3.4 the testing matrix for the text pipeline is Thai, CJK, Arabic
// (RTL), Hangul, Devanagari, emoji ZWJ. One block covers all of them.
const SAMPLE_TEXT: &str = "\
LightEditor — M0 spike\n\
\n\
สวัสดีชาวโลก  ·  你好,世界  ·  مرحبا بالعالم\n\
안녕하세요 세계  ·  नमस्ते दुनिया\n\
🇹🇭 🌏 🚀 👨‍👩‍👧‍👦\n\
\n\
The quick brown fox jumps over the lazy dog.\n\
";

struct State {
    window: Arc<Window>,
    gpu: GpuContext,
    text: TextStack,

    // Latency baseline (spec §8): rolling 1-second window.
    frame_count: u64,
    last_report: Instant,
    last_frame_us: u128,
    cold_start: Option<Instant>,
}

impl State {
    fn new(window: Arc<Window>, cold_start: Instant) -> Self {
        let size = window.inner_size();
        let gpu = GpuContext::new(window.clone());
        let text = TextStack::new(
            &gpu.device,
            &gpu.queue,
            gpu.format(),
            size.width as f32 - TEXT_PADDING,
            size.height as f32 - TEXT_PADDING,
            SAMPLE_TEXT,
        );

        Self {
            window,
            gpu,
            text,
            frame_count: 0,
            last_report: Instant::now(),
            last_frame_us: 0,
            cold_start: Some(cold_start),
        }
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        self.gpu.resize(size.width, size.height);
        self.text.set_size(
            size.width as f32 - TEXT_PADDING,
            size.height as f32 - TEXT_PADDING,
        );
    }

    fn render(&mut self) {
        let frame_start = Instant::now();

        self.text.viewport.update(
            &self.gpu.queue,
            Resolution {
                width: self.gpu.surface_config.width,
                height: self.gpu.surface_config.height,
            },
        );

        self.text
            .renderer
            .prepare(
                &self.gpu.device,
                &self.gpu.queue,
                &mut self.text.font_system,
                &mut self.text.atlas,
                &self.text.viewport,
                [TextArea {
                    buffer: &self.text.buffer,
                    left: TEXT_INSET,
                    top: TEXT_INSET,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: 0,
                        top: 0,
                        right: self.gpu.surface_config.width as i32,
                        bottom: self.gpu.surface_config.height as i32,
                    },
                    default_color: Color::rgb(238, 238, 238),
                    custom_glyphs: &[],
                }],
                &mut self.text.swash_cache,
            )
            .expect("text prepare failed");

        let Some(frame) = self.gpu.acquire() else {
            return;
        };
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("clear + text"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color {
                            r: 0.02,
                            g: 0.02,
                            b: 0.04,
                            a: 1.0,
                        }),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });
            self.text
                .renderer
                .render(&self.text.atlas, &self.text.viewport, &mut pass)
                .expect("text render failed");
        }
        self.gpu.queue.submit(Some(encoder.finish()));
        frame.present();
        self.text.atlas.trim();

        self.last_frame_us = frame_start.elapsed().as_micros();
        self.frame_count += 1;

        // First-frame report covers the cold-start budget (spec §8: target <100ms).
        if let Some(start) = self.cold_start.take() {
            log::info!(
                "first frame presented in {:.1}ms (cold start budget: 100ms target / 250ms hard)",
                start.elapsed().as_secs_f32() * 1000.0
            );
        }

        if self.last_report.elapsed().as_secs() >= 1 {
            let elapsed = self.last_report.elapsed().as_secs_f32();
            log::info!(
                "{:.1} fps · last frame {:.2}ms (target: 16ms, hard limit: 33ms)",
                self.frame_count as f32 / elapsed,
                self.last_frame_us as f32 / 1000.0,
            );
            self.frame_count = 0;
            self.last_report = Instant::now();
        }
    }
}

struct App {
    cold_start: Instant,
    state: Option<State>,
}

impl App {
    fn new() -> Self {
        Self {
            cold_start: Instant::now(),
            state: None,
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("LightEditor — M0 spike")
            .with_inner_size(PhysicalSize::new(WINDOW_WIDTH, WINDOW_HEIGHT));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );
        self.state = Some(State::new(window, self.cold_start));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => {
                log::info!("close requested — exiting");
                event_loop.exit();
            }
            WindowEvent::Resized(size) => state.resize(size),
            WindowEvent::ScaleFactorChanged { .. } => {
                state.resize(state.window.inner_size());
            }
            WindowEvent::RedrawRequested => {
                state.render();
                state.window.request_redraw();
            }
            _ => {}
        }
    }
}

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,wgpu_core=warn,wgpu_hal=warn,naga=warn"),
    )
    .init();

    let event_loop = EventLoop::new().expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new();
    event_loop.run_app(&mut app).expect("event loop failed");
}
