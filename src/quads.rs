//! Minimal colored-quad pipeline used to draw focus borders and the cursor on
//! top of the text. Builds vertices CPU-side each frame and uploads them in
//! one shot — quad counts are tiny (a handful per visible pane), so this is
//! more than fast enough.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

const INITIAL_VBUF_CAPACITY: u64 = 4096;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 2],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Resolution {
    size: [f32; 2],
    _pad: [f32; 2],
}

pub struct QuadRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    resolution_buf: wgpu::Buffer,
    vbuf: wgpu::Buffer,
    vbuf_capacity: u64,
    verts: Vec<Vertex>,
}

impl QuadRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dracoshell quad shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("quads.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dracoshell quad bgl"),
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

        let resolution_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("dracoshell quad resolution"),
            contents: bytemuck::bytes_of(&Resolution {
                size: [1.0, 1.0],
                _pad: [0.0, 0.0],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dracoshell quad bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: resolution_buf.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dracoshell quad pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 8,
                    shader_location: 1,
                },
            ],
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dracoshell quad pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                buffers: &[vertex_layout],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dracoshell quad vbuf"),
            size: INITIAL_VBUF_CAPACITY,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind_group_layout,
            bind_group,
            resolution_buf,
            vbuf,
            vbuf_capacity: INITIAL_VBUF_CAPACITY,
            verts: Vec::new(),
        }
    }

    pub fn begin(&mut self) {
        self.verts.clear();
    }

    /// Filled rectangle in pixel coordinates (origin top-left).
    pub fn quad(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let v = |x, y| Vertex { pos: [x, y], color };
        self.verts.push(v(x, y));
        self.verts.push(v(x + w, y));
        self.verts.push(v(x + w, y + h));
        self.verts.push(v(x, y));
        self.verts.push(v(x + w, y + h));
        self.verts.push(v(x, y + h));
    }

    /// Hollow rectangle outline of thickness `t` (drawn inside the rect).
    pub fn border(&mut self, x: f32, y: f32, w: f32, h: f32, t: f32, color: [f32; 4]) {
        self.quad(x, y, w, t, color); // top
        self.quad(x, y + h - t, w, t, color); // bottom
        self.quad(x, y + t, t, h - 2.0 * t, color); // left
        self.quad(x + w - t, y + t, t, h - 2.0 * t, color); // right
    }

    pub fn flush(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resolution: [f32; 2],
        pass: &mut wgpu::RenderPass<'_>,
    ) {
        if self.verts.is_empty() {
            return;
        }
        queue.write_buffer(
            &self.resolution_buf,
            0,
            bytemuck::bytes_of(&Resolution {
                size: resolution,
                _pad: [0.0, 0.0],
            }),
        );

        let bytes: &[u8] = bytemuck::cast_slice(&self.verts);
        let needed = bytes.len() as u64;
        if needed > self.vbuf_capacity {
            let new_cap = needed.next_power_of_two();
            self.vbuf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("dracoshell quad vbuf"),
                size: new_cap,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.vbuf_capacity = new_cap;
            // Re-create bind group is not needed; resolution buffer is separate.
            // bind_group_layout reuse keeps things simple.
            let _ = &self.bind_group_layout;
        }
        queue.write_buffer(&self.vbuf, 0, bytes);

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vbuf.slice(..needed));
        pass.draw(0..self.verts.len() as u32, 0..1);
    }
}
