//! Custom monospace text renderer. Each glyph is rasterized exactly once
//! into a single grayscale atlas texture and then blitted at integer cell
//! positions via instanced quads. This is the approach used by foot,
//! ghostty and kitty — no subpixel positioning, no per-frame shaping, so
//! every "P" looks identical to every other "P".

use std::collections::HashMap;

use anyhow::Result;
use bytemuck::{Pod, Zeroable};
use fontdue::{Font, FontSettings, Metrics};

use crate::colors::Color;

const ATLAS_W: u32 = 1024;
const ATLAS_H: u32 = 1024;

#[derive(Clone, Copy, Debug)]
pub struct CellMetrics {
    pub cell_w: f32,
    pub cell_h: f32,
    /// Distance from cell top to glyph baseline.
    pub baseline: f32,
}

#[derive(Clone, Copy)]
struct GlyphEntry {
    /// Atlas coords in pixels (top-left).
    atlas_x: u32,
    atlas_y: u32,
    /// Bitmap dimensions.
    width: u32,
    height: u32,
    /// Offset from cell origin (baseline-relative).
    bearing_x: i32,
    bearing_y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Default)]
pub struct GlyphInstance {
    /// Pixel position of glyph top-left.
    pub pos: [f32; 2],
    /// Glyph bitmap size in pixels.
    pub size: [f32; 2],
    /// UV min/max in 0..1 atlas space.
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    /// Foreground color in linear RGBA.
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Resolution {
    size: [f32; 2],
    _pad: [f32; 2],
}

pub struct TextRenderer {
    font: Font,
    font_size: f32,
    metrics: CellMetrics,
    glyphs: HashMap<char, Option<GlyphEntry>>,
    atlas_texture: wgpu::Texture,
    atlas_view: wgpu::TextureView,
    shelf_x: u32,
    shelf_y: u32,
    shelf_row_height: u32,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    resolution_buf: wgpu::Buffer,
    instance_buf: wgpu::Buffer,
    instance_capacity: u64,
    quad_vbuf: wgpu::Buffer,
    instances: Vec<GlyphInstance>,
}

impl TextRenderer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        font_bytes: &[u8],
        font_size: f32,
    ) -> Result<Self> {
        let font = Font::from_bytes(font_bytes, FontSettings::default())
            .map_err(|e| anyhow::anyhow!("load font: {e}"))?;

        // Probe metrics with a representative glyph (M).
        let probe: Metrics = font.metrics('M', font_size);
        let line_metrics = font
            .horizontal_line_metrics(font_size)
            .ok_or_else(|| anyhow::anyhow!("font has no horizontal line metrics"))?;
        let cell_w = probe.advance_width.round().max(1.0);
        let cell_h = (line_metrics.new_line_size).round().max(1.0);
        let baseline = line_metrics.ascent.round();
        let cell_metrics = CellMetrics {
            cell_w,
            cell_h,
            baseline,
        };

        // Atlas texture (R8 grayscale — alpha channel of each glyph).
        let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dracoshell glyph atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_W,
                height: ATLAS_H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("dracoshell glyph sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        // Bind group: resolution UBO + atlas texture + sampler.
        use wgpu::util::DeviceExt;
        let resolution_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("dracoshell text resolution"),
            contents: bytemuck::bytes_of(&Resolution {
                size: [1.0, 1.0],
                _pad: [0.0, 0.0],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dracoshell text bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
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
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dracoshell text bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: resolution_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dracoshell text shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("text.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dracoshell text pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        // Quad: 6 verts × 2 floats = unit square (0..1, 0..1) as two triangles.
        let quad_data: [f32; 12] = [
            0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0,
        ];
        let quad_vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("dracoshell text quad"),
            contents: bytemuck::cast_slice(&quad_data),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let quad_layout = wgpu::VertexBufferLayout {
            array_stride: 2 * 4,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            }],
        };
        let inst_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GlyphInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 1,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 8,
                    shader_location: 2,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 16,
                    shader_location: 3,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 24,
                    shader_location: 4,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 32,
                    shader_location: 5,
                },
            ],
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dracoshell text pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                buffers: &[quad_layout, inst_layout],
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

        let initial_capacity = 4096u64;
        let instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dracoshell text instances"),
            size: initial_capacity * std::mem::size_of::<GlyphInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let _ = queue; // queue not needed during setup (atlas filled lazily)

        Ok(Self {
            font,
            font_size,
            metrics: cell_metrics,
            glyphs: HashMap::new(),
            atlas_texture,
            atlas_view,
            shelf_x: 0,
            shelf_y: 0,
            shelf_row_height: 0,
            pipeline,
            bind_group,
            resolution_buf,
            instance_buf,
            instance_capacity: initial_capacity,
            quad_vbuf,
            instances: Vec::new(),
        })
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    pub fn begin(&mut self) {
        self.instances.clear();
    }

    /// Push a glyph at integer cell position. `col`/`row` are 0-based.
    /// `origin` is the pixel coordinate of the cell-grid (0,0) corner.
    pub fn push_cell(
        &mut self,
        queue: &wgpu::Queue,
        c: char,
        col: u32,
        row: u32,
        origin: [f32; 2],
        color: Color,
    ) {
        if c == ' ' || c == '\0' {
            return;
        }
        let entry = match self.ensure_glyph(queue, c) {
            Some(e) => e,
            None => return,
        };
        let cell_x = origin[0] + col as f32 * self.metrics.cell_w;
        let cell_y = origin[1] + row as f32 * self.metrics.cell_h;
        // Glyph top-left in window pixels.
        let gx = cell_x + entry.bearing_x as f32;
        let gy = cell_y + self.metrics.baseline - entry.bearing_y as f32;
        self.instances.push(GlyphInstance {
            pos: [gx, gy],
            size: [entry.width as f32, entry.height as f32],
            uv_min: [
                entry.atlas_x as f32 / ATLAS_W as f32,
                entry.atlas_y as f32 / ATLAS_H as f32,
            ],
            uv_max: [
                (entry.atlas_x + entry.width) as f32 / ATLAS_W as f32,
                (entry.atlas_y + entry.height) as f32 / ATLAS_H as f32,
            ],
            color: color.to_linear_f32(),
        });
    }

    pub fn flush(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resolution: [f32; 2],
        pass: &mut wgpu::RenderPass<'_>,
    ) {
        if self.instances.is_empty() {
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

        let needed = (self.instances.len() as u64) * std::mem::size_of::<GlyphInstance>() as u64;
        if self.instances.len() as u64 > self.instance_capacity {
            let new_cap = (self.instances.len() as u64).next_power_of_two();
            self.instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("dracoshell text instances"),
                size: new_cap * std::mem::size_of::<GlyphInstance>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.instance_capacity = new_cap;
        }
        queue.write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(&self.instances));

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.quad_vbuf.slice(..));
        pass.set_vertex_buffer(1, self.instance_buf.slice(..needed));
        pass.draw(0..6, 0..self.instances.len() as u32);
    }

    fn ensure_glyph(&mut self, queue: &wgpu::Queue, c: char) -> Option<GlyphEntry> {
        if let Some(slot) = self.glyphs.get(&c) {
            return *slot;
        }
        let (metrics, bitmap) = self.font.rasterize(c, self.font_size);
        let entry = if metrics.width == 0 || metrics.height == 0 {
            None
        } else {
            self.allocate_and_upload(queue, &metrics, &bitmap)
        };
        self.glyphs.insert(c, entry);
        entry
    }

    fn allocate_and_upload(
        &mut self,
        queue: &wgpu::Queue,
        metrics: &Metrics,
        bitmap: &[u8],
    ) -> Option<GlyphEntry> {
        let w = metrics.width as u32;
        let h = metrics.height as u32;
        if w > ATLAS_W || h > ATLAS_H {
            return None;
        }
        if self.shelf_x + w > ATLAS_W {
            self.shelf_x = 0;
            self.shelf_y += self.shelf_row_height;
            self.shelf_row_height = 0;
        }
        if self.shelf_y + h > ATLAS_H {
            log::warn!("glyph atlas full");
            return None;
        }
        let atlas_x = self.shelf_x;
        let atlas_y = self.shelf_y;
        self.shelf_x += w;
        self.shelf_row_height = self.shelf_row_height.max(h);

        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.atlas_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: atlas_x,
                    y: atlas_y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bitmap,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );

        Some(GlyphEntry {
            atlas_x,
            atlas_y,
            width: w,
            height: h,
            bearing_x: metrics.xmin,
            bearing_y: metrics.ymin + metrics.height as i32,
        })
    }
}
