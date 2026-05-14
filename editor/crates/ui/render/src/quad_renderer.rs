//! Draws the [`Scene`]'s `Quad` primitives through wgpu (spec §3.2).
//!
//! Walks the retained scene graph each frame, turns every `Quad` node into
//! two triangles in logical-pixel space, and draws them with a small WGSL
//! shader that maps pixels to clip space via a viewport uniform.
//!
//! Text and other primitives are drawn by their own renderers; this one only
//! understands solid-color rectangles.

use bytemuck::{Pod, Zeroable};
use editor_ui_scene::{Primitive, Scene, SceneNode};

/// A single vertex: logical-pixel position + RGBA color (0.0–1.0).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct QuadVertex {
    position: [f32; 2],
    color: [f32; 4],
}

/// Viewport size uniform. Padded to 16 bytes — the minimum uniform alignment.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct ViewportUniform {
    size: [f32; 2],
    _pad: [f32; 2],
}

/// Initial vertex-buffer capacity, in vertices (6 per quad).
const INITIAL_CAPACITY: usize = 384;

/// Renders the `Quad` primitives of a [`Scene`].
pub struct QuadRenderer {
    pipeline: wgpu::RenderPipeline,
    viewport_buffer: wgpu::Buffer,
    viewport_bind_group: wgpu::BindGroup,
    vertex_buffer: wgpu::Buffer,
    /// Capacity of `vertex_buffer`, in vertices.
    capacity: usize,
    /// Vertices written by the last `prepare`.
    vertex_count: u32,
}

impl QuadRenderer {
    /// Build the pipeline for a surface of the given `format`.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("quad shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/quad.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quad viewport bind group layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let viewport_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quad viewport uniform"),
            size: std::mem::size_of::<ViewportUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let viewport_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("quad viewport bind group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: viewport_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("quad pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<QuadVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: std::mem::size_of::<[f32; 2]>() as u64,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quad pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[vertex_layout],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let vertex_buffer = Self::make_vertex_buffer(device, INITIAL_CAPACITY);

        Self {
            pipeline,
            viewport_buffer,
            viewport_bind_group,
            vertex_buffer,
            capacity: INITIAL_CAPACITY,
            vertex_count: 0,
        }
    }

    /// Walk `scene`, turn every `Quad` into triangles, and upload them along
    /// with the current viewport size. Call once per frame before [`render`].
    ///
    /// [`render`]: QuadRenderer::render
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
        viewport_width: f32,
        viewport_height: f32,
    ) {
        queue.write_buffer(
            &self.viewport_buffer,
            0,
            bytemuck::bytes_of(&ViewportUniform {
                size: [viewport_width, viewport_height],
                _pad: [0.0, 0.0],
            }),
        );

        let mut vertices = Vec::new();
        collect_quads(scene.root(), 0.0, 0.0, &mut vertices);

        if vertices.len() > self.capacity {
            // Grow to the next power of two so resizes are rare.
            self.capacity = vertices.len().next_power_of_two();
            self.vertex_buffer = Self::make_vertex_buffer(device, self.capacity);
        }
        self.vertex_count = vertices.len() as u32;
        if !vertices.is_empty() {
            queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&vertices));
        }
    }

    /// Draw the quads prepared by the last [`prepare`] call into `pass`.
    ///
    /// [`prepare`]: QuadRenderer::prepare
    pub fn render(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.vertex_count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.viewport_bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.draw(0..self.vertex_count, 0..1);
    }

    fn make_vertex_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quad vertices"),
            size: (capacity * std::mem::size_of::<QuadVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }
}

/// Recursively emit triangles for every `Quad` node, accumulating the
/// parent-relative bounds into absolute logical-pixel coordinates.
fn collect_quads(node: &SceneNode, origin_x: f32, origin_y: f32, out: &mut Vec<QuadVertex>) {
    let abs = node.bounds().translated(origin_x, origin_y);

    if let Primitive::Quad { color } = node.primitive() {
        let c = color.to_f32_array();
        let (x0, y0) = (abs.min_x(), abs.min_y());
        let (x1, y1) = (abs.max_x(), abs.max_y());
        let tl = QuadVertex {
            position: [x0, y0],
            color: c,
        };
        let tr = QuadVertex {
            position: [x1, y0],
            color: c,
        };
        let bl = QuadVertex {
            position: [x0, y1],
            color: c,
        };
        let br = QuadVertex {
            position: [x1, y1],
            color: c,
        };
        // two triangles, counter-clockwise
        out.extend_from_slice(&[tl, bl, tr, tr, bl, br]);
    }

    for child in node.children() {
        collect_quads(child, abs.min_x(), abs.min_y(), out);
    }
}
