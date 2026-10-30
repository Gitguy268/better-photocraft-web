//! Liquid Glass on the GPU: the blurred backdrop behind the translucent glass themes, and the
//! panes under floating surfaces (`theme::Glass`, `crate::glass`).
//!
//! Once per change (not per frame) the backdrop is rebuilt at a quarter of the window's size in
//! points, fragment shaders only (so it also runs on WebGL2):
//! 1. *Ambient*: the document drawn to fill a 16² texture through its mip chain, i.e. its regional
//!    average colours. Stretched over the window and mixed with the theme's ambient colour it is
//!    the colour that panels beside the canvas diffuse.
//! 2. *Scene*: the ambient field, then the canvas view exactly where it is on screen (the canvas
//!    tile shader with its view uniforms scaled down). It is not clipped to the canvas area, so a
//!    zoomed-in image continues under the panels.
//! 3. *Blur*: a dual-filter blur (Marius Bjørge's "dual Kawase": 5-tap downsamples, 8-tap
//!    upsamples) over a chain of separate half-size textures.
//!
//! Two paint callbacks sample the result: [`backdrop`] fills the window before any panel is
//! drawn, and [`pane`] fills one floating surface (rounded-rect SDF, backdrop bent at the rim)
//! and, over the surface's tint, lights its edge. Everything else the user sees of the glass is
//! the themes' translucent token colours drawn by egui on top.

use super::*;

/// The backdrop is this many times smaller than the window, in points.
const DIVISOR: f32 = 4.0;
/// Half-size blur levels below the backdrop texture.
const LEVELS: usize = 3;
const AMBIENT_SIDE: u32 = 16;
const PARAMS: usize = 20;
const PARAMS_SIZE: u64 = PARAMS as u64 * 4;
const PANE_SIZE: u64 = 48;
/// Floating panes drawn per frame at most.
pub(crate) const MAX_PANES: usize = 48;

struct Level {
    size: [u32; 2],
    view: wgpu::TextureView,
    /// Samples this level.
    bind_group: wgpu::BindGroup,
}

/// What the cached backdrop was rendered from.
#[derive(Clone, Copy, PartialEq)]
struct Key {
    size: [u32; 2],
    params: [f32; PARAMS],
    /// Scene view uniforms, document key and content revision (`None`: no canvas view).
    view: Option<([f32; VIEW_FLOATS], u64, u32, u64)>,
}

pub(super) struct GlassGpu {
    bgl: wgpu::BindGroupLayout,
    pane_bgl: wgpu::BindGroupLayout,
    scene: wgpu::RenderPipeline,
    down: wgpu::RenderPipeline,
    up: wgpu::RenderPipeline,
    backdrop: wgpu::RenderPipeline,
    pane: wgpu::RenderPipeline,
    rim: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
    params_data: [f32; PARAMS],
    ambient: Level,
    ambient_view: ViewGpu,
    scene_view: ViewGpu,
    /// The scene before the blur, at twice the backdrop's size: what the panes' rims refract.
    sharp: Option<Level>,
    /// `levels[0]` is the backdrop; the others are the blur's scratch levels.
    levels: Vec<Level>,
    /// Device pixels per point of the frame being prepared.
    ppp: f32,
    key: Option<Key>,
    /// A backdrop callback is being prepared: canvas views record themselves in `stash`.
    pub(super) collecting: bool,
    pub(super) stash: Option<CanvasCallback>,
    panes: Vec<(wgpu::Buffer, wgpu::BindGroup, [f32; 12])>,
}

fn view_gpu(device: &wgpu::Device, res: &Resources) -> ViewGpu {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pc_glass_view"),
        size: VIEW_UNIFORM_SIZE,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("pc_glass_view"),
        layout: &res.view_bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&res.sampler) },
        ],
    });
    ViewGpu { buffer, bind_group, frame_data: [f32::NAN; VIEW_FLOATS] }
}

fn write_view(queue: &wgpu::Queue, v: &mut ViewGpu, data: [f32; VIEW_FLOATS]) {
    if v.frame_data != data {
        queue.write_buffer(&v.buffer, 0, &f32_bytes(&data));
        v.frame_data = data;
    }
}

fn begin<'a>(encoder: &'a mut wgpu::CommandEncoder, target: &wgpu::TextureView) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("pc_glass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

impl GlassGpu {
    fn new(device: &wgpu::Device, res: &Resources) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("pc_glass"), source: wgpu::ShaderSource::Wgsl(GLASS_WGSL.into()) });
        let vis = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pc_glass"),
            entries: &[texture_entry(0), sampler_entry(1), uniform_entry(2, PARAMS_SIZE, vis)],
        });
        let pane_bgl =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("pc_glass_pane"), entries: &[uniform_entry(0, PANE_SIZE, vis)] });
        let layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("pc_glass"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        // Group 0 is the blurred backdrop, group 2 the sharp scene (the same layout).
        let pane_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pc_glass_pane"),
            bind_group_layouts: &[Some(&bgl), Some(&pane_bgl), Some(&bgl)],
            immediate_size: 0,
        });
        // Premultiplied "over" with egui's alpha rule, as the canvas pipelines.
        let over = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let t = res.target;
        let full = |name: &str, fs: &str| pipeline(device, name, &layout, &module, ("vs_full", fs), t, None);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("pc_glass"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pc_glass_params"),
            size: PARAMS_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ambient = level(device, &bgl, &sampler, &params, t, [AMBIENT_SIDE; 2]);
        Self {
            scene: full("pc_glass_scene", "fs_scene"),
            down: full("pc_glass_down", "fs_down"),
            up: full("pc_glass_up", "fs_up"),
            backdrop: full("pc_glass_backdrop", "fs_backdrop"),
            pane: pipeline(device, "pc_glass_pane", &pane_layout, &module, ("vs_pane", "fs_pane"), t, Some(over)),
            rim: pipeline(device, "pc_glass_rim", &pane_layout, &module, ("vs_pane", "fs_rim"), t, Some(over)),
            bgl,
            pane_bgl,
            sampler,
            params,
            params_data: [f32::NAN; PARAMS],
            ambient,
            ambient_view: view_gpu(device, res),
            scene_view: view_gpu(device, res),
            sharp: None,
            levels: Vec::new(),
            ppp: 1.0,
            key: None,
            collecting: false,
            stash: None,
            panes: Vec::new(),
        }
    }

    /// Forget the cached backdrop (canvas colours changed without a texture upload).
    pub(super) fn invalidate(&mut self) {
        self.key = None;
    }

    /// (Re)allocate the blur chain for a `screen`-pixel window.
    fn resize(&mut self, device: &wgpu::Device, target: wgpu::TextureFormat, screen: [u32; 2], ppp: f32) {
        let div = (DIVISOR * ppp).max(1.0);
        let size = screen.map(|s| ((s as f32 / div).ceil() as u32).clamp(1, 4096));
        if self.levels.first().is_some_and(|l| l.size == size) {
            return;
        }
        self.levels = (0..=LEVELS).map(|i| level(device, &self.bgl, &self.sampler, &self.params, target, size.map(|s| (s >> i).max(1)))).collect();
        self.sharp = Some(level(device, &self.bgl, &self.sampler, &self.params, target, size.map(|s| (s * 2).min(8192))));
        self.key = None;
    }
}

fn level(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    params: &wgpu::Buffer,
    format: wgpu::TextureFormat,
    size: [u32; 2],
) -> Level {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pc_glass_level"),
        size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("pc_glass_level"),
        layout: bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
            wgpu::BindGroupEntry { binding: 2, resource: params.as_entire_binding() },
        ],
    });
    Level { size, view, bind_group }
}

/// A token colour as the shaders output it (linear when the target is an sRGB format).
fn shader_rgba(c: egui::Color32, linear: bool) -> [f32; 4] {
    let [r, g, b, a] = c.to_srgba_unmultiplied().map(|v| f32::from(v) / 255.0);
    let f = |v: f32| {
        if !linear {
            v
        } else if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    [f(r), f(g), f(b), a]
}

/// The paint callback that fills `rect` (the window) with the blurred backdrop. Add it before
/// any panel is painted.
pub(crate) fn backdrop(rect: egui::Rect, glass: crate::theme::Glass) -> egui::Shape {
    egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(rect, Backdrop { glass }))
}

/// The paint callback of one floating pane: with `rim` false the blurred backdrop inside the
/// rounded `rect` (drawn under the surface's tint), with `rim` true the lit edge (drawn over it).
/// Panes of one frame use distinct `slot`s below [`MAX_PANES`].
pub(crate) fn pane(slot: usize, rect: egui::Rect, radius: egui::CornerRadius, opacity: f32, rim: bool) -> egui::Shape {
    egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(rect.expand(1.0), Pane { slot, rect, radius, opacity, rim }))
}

struct Backdrop {
    glass: crate::theme::Glass,
}

impl CallbackTrait for Backdrop {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(res) = resources.get_mut::<Resources>() else { return Vec::new() };
        if !res.health.is_ok() {
            return Vec::new();
        }
        if res.glass.is_none() {
            res.glass = Some(Box::new(GlassGpu::new(device, res)));
        }
        let (target, linear) = (res.target, res.out_linear);
        let Some(g) = res.glass.as_mut() else { return Vec::new() };
        let ppp = screen.pixels_per_point.max(0.1);
        g.resize(device, target, screen.size_in_pixels, ppp);
        let [w, h] = screen.size_in_pixels.map(|v| v as f32);
        let (ambient, edge) = (shader_rgba(self.glass.ambient, linear), shader_rgba(self.glass.edge, linear));
        let finite = |v: f32, lo: f32, hi: f32| if v.is_finite() { v.clamp(lo, hi) } else { lo };
        #[rustfmt::skip]
        let data = [
            w, h, ppp, finite(self.glass.blur, 0.5, 4.0),
            ambient[0], ambient[1], ambient[2], finite(self.glass.ambient_mix, 0.0, 1.0),
            finite(self.glass.saturation, 0.0, 3.0), finite(self.glass.noise, 0.0, 1.0), finite(self.glass.refraction, 0.0, 64.0) * ppp, finite(self.glass.ior, 1.0, 4.0),
            edge[0], edge[1], edge[2], edge[3],
            finite(self.glass.dispersion, 0.0, 50.0), finite(self.glass.glare, 0.0, 1.2), finite(self.glass.glare_angle, -180.0, 180.0).to_radians(), 0.0,
        ];
        if g.params_data != data {
            queue.write_buffer(&g.params, 0, &f32_bytes(&data));
            g.params_data = data;
        }
        g.ppp = ppp;
        g.collecting = true;
        g.stash = None;
        Vec::new()
    }

    /// Every canvas view of the frame has been prepared: rebuild the backdrop if it changed.
    fn finish_prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(res) = resources.get_mut::<Resources>() else { return Vec::new() };
        let Some(mut g) = res.glass.take() else { return Vec::new() };
        g.collecting = false;
        if res.health.is_ok() {
            render(&mut g, res, queue, encoder);
        }
        res.glass = Some(g);
        Vec::new()
    }

    fn paint(&self, info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &CallbackResources) {
        let Some(res) = resources.get::<Resources>() else { return };
        let (Some(g), true) = (res.glass.as_ref(), res.health.is_ok()) else { return };
        let Some(level) = g.levels.first() else { return };
        if !screen_scissor(&info, pass) {
            return;
        }
        pass.set_pipeline(&g.backdrop);
        pass.set_bind_group(0, &level.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

/// Whole-screen viewport with the callback's clip rect as the scissor; false when nothing shows.
fn screen_scissor(info: &egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>) -> bool {
    let [sw, sh] = info.screen_size_px;
    let clip = info.clip_rect_in_pixels();
    if clip.width_px <= 0 || clip.height_px <= 0 || sw == 0 || sh == 0 {
        return false;
    }
    pass.set_viewport(0.0, 0.0, sw as f32, sh as f32, 0.0, 1.0);
    pass.set_scissor_rect(clip.left_px.max(0) as u32, clip.top_px.max(0) as u32, clip.width_px as u32, clip.height_px as u32);
    true
}

fn render(g: &mut GlassGpu, res: &Resources, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder) {
    let (Some(size), Some(sharp_size)) = (g.levels.first().map(|l| l.size), g.sharp.as_ref().map(|l| l.size)) else { return };
    // The canvas view of this frame, if its document is on the GPU.
    let shown = g.stash.take().and_then(|cb| {
        let doc = res.docs.get(&cb.params.doc).filter(|d| d.size == cb.params.doc_size)?;
        Some((cb, doc))
    });
    // The scene is drawn at the sharp texture's size; the blur chain starts below it.
    let scene_ppp = g.ppp / (DIVISOR * g.ppp).max(1.0) * sharp_size[0] as f32 / size[0].max(1) as f32;
    let view = shown.as_ref().map(|(cb, doc)| {
        let p = &cb.params;
        (cb.uniforms(sharp_size, scene_ppp, res.out_linear, &res.style), p.doc, p.output, doc.rev.load(std::sync::atomic::Ordering::Relaxed))
    });
    let key = Key { size, params: g.params_data, view };
    if g.key == Some(key) {
        return;
    }
    g.key = Some(key);
    let draw_tiles = |pass: &mut wgpu::RenderPass<'_>, view: &ViewGpu, cb: &CanvasCallback, doc: &DocTextures| {
        pass.set_pipeline(&res.tile_pipeline);
        pass.set_bind_group(0, &view.bind_group, &[]);
        pass.set_bind_group(2, res.luts.get(&(cb.params.doc, cb.params.output)).unwrap_or(&res.identity_lut), &[]);
        for t in &doc.tiles {
            pass.set_bind_group(1, &t.bind_group, &[]);
            pass.draw(0..6, 0..1);
        }
    };
    // 1. Ambient: the whole document covering a tiny texture (left transparent without one).
    {
        if let Some((cb, _)) = &shown {
            let [dw, dh] = cb.params.doc_size.map(|v| v.max(1) as f32);
            let side = AMBIENT_SIDE as f32;
            let fill = CanvasCallback {
                rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(side, side)),
                params: ViewParams { zoom: (side / dw).max(side / dh), center: [dw / 2.0, dh / 2.0], rotation: 0.0, pixel_grid: false, ..cb.params },
            };
            write_view(queue, &mut g.ambient_view, fill.uniforms([AMBIENT_SIDE; 2], 1.0, res.out_linear, &res.style));
        }
        let mut pass = begin(encoder, &g.ambient.view);
        if let Some((cb, doc)) = &shown {
            draw_tiles(&mut pass, &g.ambient_view, cb, doc);
        }
    }
    // 2. Scene: the ambient field, then the canvas view where it is on screen, kept sharp.
    {
        if let Some((data, ..)) = view {
            write_view(queue, &mut g.scene_view, data);
        }
        let Some(target) = g.sharp.as_ref() else { return };
        let mut pass = begin(encoder, &target.view);
        pass.set_pipeline(&g.scene);
        pass.set_bind_group(0, &g.ambient.bind_group, &[]);
        pass.draw(0..3, 0..1);
        if let Some((cb, doc)) = &shown {
            draw_tiles(&mut pass, &g.scene_view, cb, doc);
        }
    }
    // 3. Blur: the sharp scene down into the backdrop, down the chain and back up.
    if let (Some(src), Some(dst)) = (g.sharp.as_ref(), g.levels.first()) {
        let mut pass = begin(encoder, &dst.view);
        pass.set_pipeline(&g.down);
        pass.set_bind_group(0, &src.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
    let n = g.levels.len();
    let steps = (1..n).map(|i| (i - 1, i, &g.down)).chain((1..n).rev().map(|i| (i, i - 1, &g.up)));
    for (from, to, pipeline) in steps {
        let (Some(src), Some(dst)) = (g.levels.get(from), g.levels.get(to)) else { continue };
        let mut pass = begin(encoder, &dst.view);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &src.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

struct Pane {
    slot: usize,
    rect: egui::Rect,
    radius: egui::CornerRadius,
    opacity: f32,
    rim: bool,
}

impl CallbackTrait for Pane {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        // The body and the rim of a pane share one uniform slot: the body writes it.
        let Some(g) = resources.get_mut::<Resources>().and_then(|res| res.glass.as_mut()) else { return Vec::new() };
        if self.rim || self.slot >= MAX_PANES {
            return Vec::new();
        }
        while g.panes.len() <= self.slot {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pc_glass_pane"),
                size: PANE_SIZE,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pc_glass_pane"),
                layout: &g.pane_bgl,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
            });
            g.panes.push((buffer, bind_group, [f32::NAN; 12]));
        }
        let ppp = screen.pixels_per_point;
        let (r, c) = (self.rect, self.radius);
        #[rustfmt::skip]
        let data = [
            r.min.x * ppp, r.min.y * ppp, r.max.x * ppp, r.max.y * ppp,
            f32::from(c.nw) * ppp, f32::from(c.ne) * ppp, f32::from(c.se) * ppp, f32::from(c.sw) * ppp,
            self.opacity.clamp(0.0, 1.0), 0.0, 0.0, 0.0,
        ];
        if let Some((buffer, _, last)) = g.panes.get_mut(self.slot)
            && *last != data
        {
            queue.write_buffer(buffer, 0, &f32_bytes(&data));
            *last = data;
        }
        Vec::new()
    }

    fn paint(&self, info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &CallbackResources) {
        let Some(res) = resources.get::<Resources>() else { return };
        let (Some(g), true) = (res.glass.as_ref(), res.health.is_ok()) else { return };
        let (Some(level), Some(sharp), Some((_, pane, _))) = (g.levels.first(), g.sharp.as_ref(), g.panes.get(self.slot)) else { return };
        if !screen_scissor(&info, pass) {
            return;
        }
        pass.set_pipeline(if self.rim { &g.rim } else { &g.pane });
        pass.set_bind_group(0, &level.bind_group, &[]);
        pass.set_bind_group(1, pane, &[]);
        pass.set_bind_group(2, &sharp.bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

pub(super) const GLASS_WGSL: &str = r#"
struct Params {
    a: vec4<f32>, // screen w, h (device px), pixels per point, blur spread
    b: vec4<f32>, // ambient rgb, how much of the document's colours washes into it
    c: vec4<f32>, // saturation, grain, rim thickness (device px), refractive index
    d: vec4<f32>, // edge light rgb, strength
    f: vec4<f32>, // dispersion, glare strength, glare angle (radians), unused
};
struct Pane {
    rect: vec4<f32>,  // x0, y0, x1, y1 (device px)
    radii: vec4<f32>, // nw, ne, se, sw (device px)
    e: vec4<f32>,     // opacity, unused
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> params: Params;
@group(1) @binding(0) var<uniform> pane: Pane;
@group(2) @binding(0) var sharp: texture_2d<f32>;

struct V { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs_full(@builtin(vertex_index) vi: u32) -> V {
    let uv = vec2(f32((vi << 1u) & 2u), f32(vi & 2u));
    return V(vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0), uv);
}

fn tap(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(src, samp, uv, 0.0);
}

// The ambient field: the document's regional colours (a tiny texture, smoothed by four taps)
// mixed into the theme's ambient colour, lit a little from the top left.
@fragment
fn fs_scene(in: V) -> @location(0) vec4<f32> {
    let h = 0.5 / vec2<f32>(textureDimensions(src));
    let c = (tap(in.uv + vec2(-h.x, -h.y)) + tap(in.uv + vec2(h.x, -h.y)) + tap(in.uv + vec2(-h.x, h.y)) + tap(in.uv + h)) * 0.25;
    let light = mix(1.12, 0.9, (in.uv.x + in.uv.y) * 0.5);
    return vec4(mix(params.b.rgb, c.rgb, params.b.w * c.a) * light, 1.0);
}

// Dual-filter blur: each downsample averages the centre and four diagonal taps, each upsample
// eight taps on a diamond.
@fragment
fn fs_down(in: V) -> @location(0) vec4<f32> {
    let h = params.a.w / vec2<f32>(textureDimensions(src));
    let sum = tap(in.uv) * 4.0 + tap(in.uv - h) + tap(in.uv + h) + tap(in.uv + vec2(h.x, -h.y)) + tap(in.uv + vec2(-h.x, h.y));
    return vec4(sum.rgb / 8.0, 1.0);
}

@fragment
fn fs_up(in: V) -> @location(0) vec4<f32> {
    let h = params.a.w * 0.5 / vec2<f32>(textureDimensions(src));
    var sum = tap(in.uv + vec2(-h.x * 2.0, 0.0)) + tap(in.uv + vec2(h.x * 2.0, 0.0)) + tap(in.uv + vec2(0.0, h.y * 2.0)) + tap(in.uv + vec2(0.0, -h.y * 2.0));
    sum += (tap(in.uv + vec2(-h.x, h.y)) + tap(in.uv + h) + tap(in.uv + vec2(h.x, -h.y)) + tap(in.uv - h)) * 2.0;
    return vec4(sum.rgb / 12.0, 1.0);
}

// The backdrop at screen position `uv`: saturated so colour survives the surface tints, with a
// little grain (interleaved gradient noise) that also dithers the smooth gradients.
fn glass(uv: vec2<f32>, px: vec2<f32>) -> vec3<f32> {
    return grade(tap(clamp(uv, vec2(0.0), vec2(1.0))).rgb, px);
}

fn grade(c: vec3<f32>, px: vec2<f32>) -> vec3<f32> {
    let l = dot(c, vec3(0.2126, 0.7152, 0.0722));
    let grain = fract(52.9829189 * fract(dot(floor(px), vec2(0.06711056, 0.00583715)))) - 0.5;
    return clamp(mix(vec3(l), c, params.c.x) + grain * params.c.y * 0.03, vec3(0.0), vec3(1.0));
}

@fragment
fn fs_backdrop(in: V) -> @location(0) vec4<f32> {
    return vec4(glass(in.uv, in.pos.xy), 1.0);
}

// --- floating panes ------------------------------------------------------------------------

@vertex
fn vs_pane(@builtin(vertex_index) vi: u32) -> V {
    var c = array<vec2<f32>, 6>(vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0));
    let p = mix(pane.rect.xy - 1.0, pane.rect.zw + 1.0, c[vi]);
    let n = p / params.a.xy * 2.0 - 1.0;
    return V(vec4(n.x, -n.y, 0.0, 1.0), p / params.a.xy);
}

// Signed distance to the pane's rounded rectangle (negative inside), `p` from its centre.
fn sd_pane(p: vec2<f32>) -> f32 {
    let half = (pane.rect.zw - pane.rect.xy) * 0.5;
    let side = select(pane.radii.xw, pane.radii.yz, p.x > 0.0); // top, bottom radius of this side
    let r = min(select(side.x, side.y, p.y > 0.0), min(half.x, half.y));
    let q = abs(p) - half + r;
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2(0.0))) - r;
}

fn pane_normal(p: vec2<f32>) -> vec2<f32> {
    let g = vec2(sd_pane(p + vec2(1.0, 0.0)) - sd_pane(p - vec2(1.0, 0.0)), sd_pane(p + vec2(0.0, 1.0)) - sd_pane(p - vec2(0.0, 1.0)));
    return g / max(length(g), 1e-4);
}

// The lens optics below follow liquid-glass-studio (MIT, (c) 2024 Charles Yin,
// github.com/iyinchao/liquid-glass-studio): Snell refraction over the rim's depth, colour
// dispersion, and Fresnel and glare bands. Its shape merging and springs are left out. Its
// licence notice is in ATTRIBUTION.md.

// How far light bends at signed distance `d` (device px, negative inside) into a rim
// `params.c.z` thick: the rim is a quarter-round lens, steepest at the edge, 0 past it.
fn bend(d: f32) -> f32 {
    let x = 1.0 + d / max(params.c.z, 1.0);
    if (x <= 0.0 || d > 0.0) {
        return 0.0;
    }
    let ti = asin(clamp(x * x, 0.0, 1.0));
    let tt = asin(clamp(sin(ti) / max(params.c.w, 1.0), -1.0, 1.0));
    return tan(ti - tt);
}

// What the pane shows at `px`: inside the rim the scene bent towards the pane's middle, each
// channel by a slightly different amount, going from sharp at the edge to blurred inwards;
// past the rim the blurred backdrop.
fn lens(px: vec2<f32>, d: f32, n: vec2<f32>) -> vec3<f32> {
    let k = bend(d);
    if (k <= 0.0) {
        return glass(px / params.a.xy, px);
    }
    let w = max(params.c.z, 1.0);
    let o = -n * k * w;
    let spread = 0.02 * params.f.x;
    let ur = clamp((px + o * (1.0 + spread)) / params.a.xy, vec2(0.0), vec2(1.0));
    let ug = clamp((px + o) / params.a.xy, vec2(0.0), vec2(1.0));
    let ub = clamp((px + o * (1.0 - spread)) / params.a.xy, vec2(0.0), vec2(1.0));
    let s = vec3(textureSampleLevel(sharp, samp, ur, 0.0).r, textureSampleLevel(sharp, samp, ug, 0.0).g, textureSampleLevel(sharp, samp, ub, 0.0).b);
    let b = vec3(tap(ur).r, tap(ug).g, tap(ub).b);
    return grade(mix(s, b, clamp(-d / w, 0.0, 1.0)), px);
}

// A band along the edge that falls off within a few points (`range` and `hard` as the studio's).
fn edge_band(d_pt: f32, range: f32, hard: f32) -> f32 {
    return clamp(pow(max(1.0 + d_pt / 1500.0 * pow(500.0 / range, 2.0) + hard, 0.0), 5.0), 0.0, 1.0);
}

// The pane's body, under the surface tint.
@fragment
fn fs_pane(in: V) -> @location(0) vec4<f32> {
    let p = in.pos.xy - (pane.rect.xy + pane.rect.zw) * 0.5;
    let d = sd_pane(p);
    let cover = clamp(0.5 - d, 0.0, 1.0) * pane.e.x;
    if (cover <= 0.0) {
        discard;
    }
    return vec4(lens(in.pos.xy, d, pane_normal(p)) * cover, cover);
}

// The pane's rim, over the surface tint (so it reads through it): the outer part of the lens,
// fading out before any text, then a Fresnel sheen and the glare, brightest on the side facing
// the light and a little weaker on the opposite side.
@fragment
fn fs_rim(in: V) -> @location(0) vec4<f32> {
    let p = in.pos.xy - (pane.rect.xy + pane.rect.zw) * 0.5;
    let d = sd_pane(p);
    let inside = clamp(0.5 - d, 0.0, 1.0) * pane.e.x;
    if (inside <= 0.0) {
        discard;
    }
    let n = pane_normal(p);
    let d_pt = min(d, 0.0) / max(params.a.z, 0.1);
    // Over the tint only in the outer 5 points, which hold no text; the rest is under it.
    let outer = 1.0 - clamp(-d_pt / 5.0, 0.0, 1.0);
    let lens_a = outer * outer * 0.7 * step(0.0001, bend(d));
    let band = edge_band(d_pt, 30.0, 0.2);
    // Most of a pane is past both the lens and the bands: skip its texture taps.
    if (lens_a <= 0.002 && band <= 0.002) {
        discard;
    }
    let fresnel = band * params.d.a * 0.8;
    // Angle of the normal with y up, turned by the light's angle; two lobes per turn.
    var ang = atan2(-n.y, n.x);
    if (ang < 0.0) {
        ang += 6.28318531;
    }
    ang = (ang - 0.78539816 + params.f.z) * 2.0;
    let far = (ang > 4.71238898 && ang < 10.99557429) || ang < -1.57079633;
    let side = select(1.2, 0.96, far);
    let glare = band * clamp(pow(max((0.5 + sin(ang) * 0.5) * side * params.f.y, 0.0), 1.1), 0.0, 1.0);
    let behind = lens(in.pos.xy, d, n);
    let shine = min(behind * 1.4 + vec3(0.45), vec3(1.0));
    // Premultiplied: the lens, then the sheen and the glare over it.
    var rgb = behind * lens_a;
    var a = lens_a;
    rgb = rgb * (1.0 - fresnel) + params.d.rgb * fresnel;
    a = a + fresnel * (1.0 - a);
    rgb = rgb * (1.0 - glare) + shine * glare;
    a = a + glare * (1.0 - a);
    a = clamp(a * inside, 0.0, 1.0);
    if (a <= 0.002) {
        discard;
    }
    return vec4(clamp(rgb * inside, vec3(0.0), vec3(a)), a);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_colours_follow_the_target_encoding() {
        let c = egui::Color32::from_rgba_unmultiplied(128, 0, 255, 128);
        let [r, _, b, a] = shader_rgba(c, false);
        assert!((r - 128.0 / 255.0).abs() < 0.01 && b == 1.0 && (a - 0.5).abs() < 0.01);
        let [r, g, b, _] = shader_rgba(c, true);
        assert!((r - 0.2158).abs() < 0.01 && g == 0.0 && b == 1.0, "linear for an sRGB target");
    }
}
