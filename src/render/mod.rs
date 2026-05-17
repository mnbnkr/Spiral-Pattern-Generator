use wasm_bindgen::prelude::*;
use wasm_bindgen::{Clamped, JsCast};
use web_sys::{
    Blob, CanvasRenderingContext2d, HtmlAnchorElement, HtmlCanvasElement, ImageData, Url,
    WebGlBuffer, WebGlProgram, WebGlRenderingContext as Gl, WebGlShader, WebGlUniformLocation,
};

use crate::math::{ArchimedeanSpiral, HexSpiral, SquareSpiral, TriangleSpiral};
use crate::protocol::{
    BoardKind, ColorState, DisplayMode, EngineSettings, ShapeKind, VertexBufferUpdate,
};

const FLOATS_PER_VERTEX: usize = 5;
const BYTES_PER_FLOAT: usize = std::mem::size_of::<f32>();
const INITIAL_VERTEX_CAPACITY: usize = 16_384;
const MAX_TRACK_POINTS: usize = 160_000;
const MAX_EXPORT_PIXELS: usize = 64_000_000;
const MAX_EXPORT_DIMENSION: u32 = 32_767;

const VERTEX_SHADER: &str = r#"
attribute vec2 a_position;
attribute vec3 a_color;

uniform vec2 u_resolution;
uniform float u_scale;
uniform float u_point_size;
uniform vec2 u_pan;

varying vec3 v_color;

void main() {
    vec2 screen = vec2(
        u_resolution.x * 0.5 + (a_position.x + u_pan.x) * u_scale,
        u_resolution.y * 0.5 - (a_position.y + u_pan.y) * u_scale
    );
    vec2 clip = vec2(
        (screen.x / u_resolution.x) * 2.0 - 1.0,
        1.0 - (screen.y / u_resolution.y) * 2.0
    );

    gl_Position = vec4(clip, 0.0, 1.0);
    gl_PointSize = max(u_point_size, 1.0);
    v_color = a_color;
}
"#;

const FRAGMENT_SHADER: &str = r#"
precision mediump float;

uniform int u_shape;
uniform float u_alpha;

varying vec3 v_color;

void main() {
    if (u_shape == 1) {
        vec2 p = gl_PointCoord * 2.0 - 1.0;
        if (dot(p, p) > 1.0) {
            discard;
        }
    } else if (u_shape == 2) {
        vec2 p = abs(gl_PointCoord * 2.0 - 1.0);
        if (p.x > 0.8660254 || p.y > 1.0 || p.x * 0.5773503 + p.y > 1.0) {
            discard;
        }
    } else if (u_shape == 3) {
        vec2 p = gl_PointCoord * 2.0 - 1.0;
        if (p.y < -0.5 || p.y > 1.0 || abs(p.x) > (1.0 - p.y) * 0.5773503) {
            discard;
        }
    }

    gl_FragColor = vec4(v_color, u_alpha);
}
"#;

pub struct CanvasRenderer {
    canvas: HtmlCanvasElement,
    gl: Gl,
    program: WebGlProgram,
    buffer: WebGlBuffer,
    position_attrib: u32,
    color_attrib: u32,
    resolution_uniform: WebGlUniformLocation,
    scale_uniform: WebGlUniformLocation,
    point_size_uniform: WebGlUniformLocation,
    pan_uniform: WebGlUniformLocation,
    shape_uniform: WebGlUniformLocation,
    alpha_uniform: WebGlUniformLocation,
    vertices: Vec<f32>,
    track_vertices: Vec<f32>,
    pending_upload: PendingUpload,
    track_upload_pending: bool,
    gpu_capacity_floats: usize,
    track_buffer: WebGlBuffer,
    settings: EngineSettings,
    color_state: ColorState,
    pan_x: f64,
    pan_y: f64,
    track_key: Option<TrackKey>,
}

impl CanvasRenderer {
    pub fn new(canvas_id: &str) -> Result<Self, JsValue> {
        let window = web_sys::window().ok_or_else(|| JsValue::from_str("window unavailable"))?;
        let document = window
            .document()
            .ok_or_else(|| JsValue::from_str("document unavailable"))?;
        let canvas = document
            .get_element_by_id(canvas_id)
            .ok_or_else(|| JsValue::from_str("canvas not found"))?
            .dyn_into::<HtmlCanvasElement>()?;
        let gl = canvas
            .get_context("webgl")?
            .ok_or_else(|| JsValue::from_str("webgl context unavailable"))?
            .dyn_into::<Gl>()?;

        let program = link_program(
            &gl,
            &compile_shader(&gl, Gl::VERTEX_SHADER, VERTEX_SHADER)?,
            &compile_shader(&gl, Gl::FRAGMENT_SHADER, FRAGMENT_SHADER)?,
        )?;
        gl.use_program(Some(&program));

        let buffer = gl
            .create_buffer()
            .ok_or_else(|| JsValue::from_str("failed to create WebGL buffer"))?;
        let position_attrib = attrib_location(&gl, &program, "a_position")?;
        let color_attrib = attrib_location(&gl, &program, "a_color")?;
        let resolution_uniform = uniform_location(&gl, &program, "u_resolution")?;
        let scale_uniform = uniform_location(&gl, &program, "u_scale")?;
        let point_size_uniform = uniform_location(&gl, &program, "u_point_size")?;
        let pan_uniform = uniform_location(&gl, &program, "u_pan")?;
        let shape_uniform = uniform_location(&gl, &program, "u_shape")?;
        let alpha_uniform = uniform_location(&gl, &program, "u_alpha")?;
        let track_buffer = gl
            .create_buffer()
            .ok_or_else(|| JsValue::from_str("failed to create WebGL track buffer"))?;

        gl.clear_color(0.031, 0.035, 0.039, 1.0);
        gl.disable(Gl::DEPTH_TEST);
        gl.enable(Gl::BLEND);
        gl.blend_func(Gl::SRC_ALPHA, Gl::ONE_MINUS_SRC_ALPHA);

        let mut renderer = Self {
            canvas,
            gl,
            program,
            buffer,
            position_attrib,
            color_attrib,
            resolution_uniform,
            scale_uniform,
            point_size_uniform,
            pan_uniform,
            shape_uniform,
            alpha_uniform,
            vertices: Vec::new(),
            track_vertices: Vec::new(),
            pending_upload: PendingUpload::Full,
            track_upload_pending: true,
            gpu_capacity_floats: 0,
            track_buffer,
            settings: EngineSettings::default(),
            color_state: ColorState::default(),
            pan_x: 0.0,
            pan_y: 0.0,
            track_key: None,
        };
        renderer.resize_to_viewport()?;
        Ok(renderer)
    }

    pub fn set_settings(&mut self, settings: EngineSettings) -> Result<(), JsValue> {
        let next_track_key = TrackKey::from_settings(&settings);
        if next_track_key != self.track_key {
            self.track_vertices = build_track_vertices(&settings);
            self.track_upload_pending = true;
            self.track_key = next_track_key;
        }
        self.settings = settings;
        self.clamp_pan_to_view();
        self.redraw()
    }

    pub fn set_color_state(&mut self, color_state: ColorState) -> Result<(), JsValue> {
        self.color_state = color_state;
        self.redraw()
    }

    pub fn clear_placements(&mut self) -> Result<(), JsValue> {
        self.vertices.clear();
        self.pending_upload = PendingUpload::Full;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
        self.redraw()
    }

    pub fn pan_by_pixels(&mut self, dx: f64, dy: f64) -> Result<(), JsValue> {
        let scale = self.world_scale(self.canvas.width() as f64, self.canvas.height() as f64);
        if scale <= f64::EPSILON {
            return Ok(());
        }

        self.pan_x += dx / scale;
        self.pan_y -= dy / scale;
        self.clamp_pan_to_view();
        self.redraw()
    }

    pub fn zoom_at(&mut self, client_x: f64, client_y: f64, delta: i32) -> Result<u8, JsValue> {
        if self.settings.display_mode != DisplayMode::PixelOneToOne || delta == 0 {
            return Ok(self.settings.zoom);
        }

        let old_zoom = self.settings.zoom.clamp(1, 16);
        let new_zoom = (old_zoom as i32 + delta).clamp(1, 16) as u8;
        if new_zoom == old_zoom {
            return Ok(old_zoom);
        }

        let window = web_sys::window().ok_or_else(|| JsValue::from_str("window unavailable"))?;
        let dpr = window.device_pixel_ratio();
        let px = client_x * dpr;
        let py = client_y * dpr;
        let width = self.canvas.width() as f64;
        let height = self.canvas.height() as f64;
        let old_scale = self.world_scale(width, height);
        let world_x = (px - width * 0.5) / old_scale - self.pan_x;
        let world_y = (height * 0.5 - py) / old_scale - self.pan_y;

        self.settings.zoom = new_zoom;
        let new_scale = self.world_scale(width, height);
        self.pan_x = (px - width * 0.5) / new_scale - world_x;
        self.pan_y = (height * 0.5 - py) / new_scale - world_y;
        self.clamp_pan_to_view();
        self.redraw()?;
        Ok(new_zoom)
    }

    pub fn apply_batch(
        &mut self,
        vertex_update: &VertexBufferUpdate,
        color_state: ColorState,
    ) -> Result<(), JsValue> {
        self.color_state = color_state;
        self.apply_vertex_update(vertex_update);
        Ok(())
    }

    pub fn apply_stats(
        &mut self,
        vertex_update: &VertexBufferUpdate,
        color_state: ColorState,
    ) -> Result<(), JsValue> {
        self.color_state = color_state;
        self.apply_vertex_update(vertex_update);
        Ok(())
    }

    fn apply_vertex_update(&mut self, vertex_update: &VertexBufferUpdate) {
        match vertex_update {
            VertexBufferUpdate::None => {}
            VertexBufferUpdate::Append(vertices) => {
                let start = self.vertices.len();
                self.vertices.extend_from_slice(vertices);
                self.pending_upload = match self.pending_upload {
                    PendingUpload::Full => PendingUpload::Full,
                    PendingUpload::Append { start_float } => PendingUpload::Append {
                        start_float: start_float.min(start),
                    },
                    PendingUpload::None => PendingUpload::Append { start_float: start },
                };
            }
            VertexBufferUpdate::Replace(vertices) => {
                self.vertices.clear();
                self.vertices.extend_from_slice(vertices);
                self.pending_upload = PendingUpload::Full;
            }
        }
    }

    pub fn resize_to_viewport(&mut self) -> Result<(), JsValue> {
        let window = web_sys::window().ok_or_else(|| JsValue::from_str("window unavailable"))?;
        let width = window
            .inner_width()?
            .as_f64()
            .ok_or_else(|| JsValue::from_str("invalid window width"))?;
        let height = window
            .inner_height()?
            .as_f64()
            .ok_or_else(|| JsValue::from_str("invalid window height"))?;
        let scale = window.device_pixel_ratio();

        self.canvas.set_width((width * scale).round() as u32);
        self.canvas.set_height((height * scale).round() as u32);
        self.canvas
            .style()
            .set_property("width", &format!("{width}px"))?;
        self.canvas
            .style()
            .set_property("height", &format!("{height}px"))?;
        self.redraw()
    }

    pub fn redraw(&mut self) -> Result<(), JsValue> {
        let width = self.canvas.width();
        let height = self.canvas.height();
        let render_state = self.render_state(width, height);

        self.gl.viewport(0, 0, width as i32, height as i32);
        self.gl.use_program(Some(&self.program));
        self.gl
            .uniform2f(Some(&self.resolution_uniform), width as f32, height as f32);
        self.gl
            .uniform1f(Some(&self.scale_uniform), render_state.scale);
        self.gl.uniform2f(
            Some(&self.pan_uniform),
            self.pan_x as f32,
            self.pan_y as f32,
        );
        self.gl.clear(Gl::COLOR_BUFFER_BIT);

        if self.settings.track_opacity > f32::EPSILON && !self.track_vertices.is_empty() {
            self.draw_track()?;
        }

        if self.vertices.is_empty() {
            self.pending_upload = PendingUpload::None;
            return Ok(());
        }

        self.gl.bind_buffer(Gl::ARRAY_BUFFER, Some(&self.buffer));
        self.sync_gpu_buffer()?;
        self.configure_vertex_attribs();
        self.gl
            .uniform1f(Some(&self.point_size_uniform), render_state.point_size);
        self.gl
            .uniform1i(Some(&self.shape_uniform), render_state.shape);
        self.gl.uniform1f(Some(&self.alpha_uniform), 1.0);
        self.gl.draw_arrays(
            Gl::POINTS,
            0,
            (self.vertices.len() / FLOATS_PER_VERTEX) as i32,
        );

        Ok(())
    }

    fn draw_track(&mut self) -> Result<(), JsValue> {
        self.gl
            .bind_buffer(Gl::ARRAY_BUFFER, Some(&self.track_buffer));
        if self.track_upload_pending {
            unsafe {
                let view = js_sys::Float32Array::view(&self.track_vertices);
                self.gl.buffer_data_with_array_buffer_view(
                    Gl::ARRAY_BUFFER,
                    &view,
                    Gl::STATIC_DRAW,
                );
            }
            self.track_upload_pending = false;
        }

        self.configure_vertex_attribs();
        self.gl.uniform1f(Some(&self.point_size_uniform), 1.0);
        self.gl.uniform1i(Some(&self.shape_uniform), 0);
        self.gl.uniform1f(
            Some(&self.alpha_uniform),
            self.settings.track_opacity.clamp(0.0, 1.0),
        );
        self.gl.draw_arrays(
            Gl::LINE_STRIP,
            0,
            (self.track_vertices.len() / FLOATS_PER_VERTEX) as i32,
        );
        Ok(())
    }

    fn configure_vertex_attribs(&self) {
        let stride = 5 * std::mem::size_of::<f32>() as i32;
        self.gl.enable_vertex_attrib_array(self.position_attrib);
        self.gl.vertex_attrib_pointer_with_i32(
            self.position_attrib,
            2,
            Gl::FLOAT,
            false,
            stride,
            0,
        );
        self.gl.enable_vertex_attrib_array(self.color_attrib);
        self.gl.vertex_attrib_pointer_with_i32(
            self.color_attrib,
            3,
            Gl::FLOAT,
            false,
            stride,
            2 * std::mem::size_of::<f32>() as i32,
        );
    }

    pub fn download_image(
        &self,
        mime_type: &str,
        filename: &str,
        resolution_scale: f64,
        encoder_quality: Option<f64>,
    ) -> Result<(), JsValue> {
        let export_canvas = self.pixel_export_canvas(resolution_scale)?;
        let window = web_sys::window().ok_or_else(|| JsValue::from_str("window unavailable"))?;
        let document = window
            .document()
            .ok_or_else(|| JsValue::from_str("document unavailable"))?;
        let anchor = document
            .create_element("a")?
            .dyn_into::<HtmlAnchorElement>()?;
        anchor.set_download(filename);

        let callback = Closure::<dyn FnMut(JsValue)>::new(move |blob_value: JsValue| {
            if blob_value.is_null() || blob_value.is_undefined() {
                web_sys::console::error_1(&JsValue::from_str("image encoder returned no blob"));
                return;
            }

            let blob = blob_value.unchecked_into::<Blob>();
            match Url::create_object_url_with_blob(&blob) {
                Ok(url) => {
                    anchor.set_href(&url);
                    anchor.click();
                    if let Err(error) = Url::revoke_object_url(&url) {
                        web_sys::console::error_1(&error);
                    }
                }
                Err(error) => web_sys::console::error_1(&error),
            }
        });
        let quality = encoder_quality.unwrap_or(0.92);
        export_canvas.to_blob_with_type_and_encoder_options(
            callback.as_ref().unchecked_ref(),
            mime_type,
            &JsValue::from_f64(quality),
        )?;
        callback.forget();
        Ok(())
    }

    fn sync_gpu_buffer(&mut self) -> Result<(), JsValue> {
        match self.pending_upload {
            PendingUpload::None => Ok(()),
            PendingUpload::Full => {
                self.ensure_gpu_capacity(self.vertices.len())?;
                self.upload_vertex_range(0, self.vertices.len())?;
                self.pending_upload = PendingUpload::None;
                Ok(())
            }
            PendingUpload::Append { start_float } => {
                let total = self.vertices.len();
                let start_float = start_float.min(total);
                let capacity_grew = self.ensure_gpu_capacity(total)?;
                if capacity_grew {
                    self.upload_vertex_range(0, total)?;
                } else {
                    self.upload_vertex_range(start_float, total)?;
                }
                self.pending_upload = PendingUpload::None;
                Ok(())
            }
        }
    }

    fn ensure_gpu_capacity(&mut self, required_floats: usize) -> Result<bool, JsValue> {
        if required_floats <= self.gpu_capacity_floats {
            return Ok(false);
        }

        let required_vertices = required_floats.div_ceil(FLOATS_PER_VERTEX);
        let capacity_vertices = required_vertices
            .next_power_of_two()
            .max(INITIAL_VERTEX_CAPACITY);
        let capacity_floats = capacity_vertices * FLOATS_PER_VERTEX;
        let capacity_bytes = capacity_floats
            .checked_mul(BYTES_PER_FLOAT)
            .and_then(|bytes| i32::try_from(bytes).ok())
            .ok_or_else(|| JsValue::from_str("WebGL vertex buffer is too large"))?;

        self.gl
            .buffer_data_with_i32(Gl::ARRAY_BUFFER, capacity_bytes, Gl::DYNAMIC_DRAW);
        self.gpu_capacity_floats = capacity_floats;
        Ok(true)
    }

    fn upload_vertex_range(&self, start_float: usize, end_float: usize) -> Result<(), JsValue> {
        if start_float >= end_float {
            return Ok(());
        }

        let offset_bytes = start_float
            .checked_mul(BYTES_PER_FLOAT)
            .and_then(|bytes| i32::try_from(bytes).ok())
            .ok_or_else(|| JsValue::from_str("WebGL vertex upload offset is too large"))?;

        unsafe {
            let view = js_sys::Float32Array::view(&self.vertices[start_float..end_float]);
            self.gl.buffer_sub_data_with_i32_and_array_buffer_view(
                Gl::ARRAY_BUFFER,
                offset_bytes,
                &view,
            );
        }
        Ok(())
    }

    fn pixel_export_canvas(&self, resolution_scale: f64) -> Result<HtmlCanvasElement, JsValue> {
        let spec = self.export_spec(resolution_scale);
        let pixel_count = spec.width as usize * spec.height as usize;
        if spec.width > MAX_EXPORT_DIMENSION || spec.height > MAX_EXPORT_DIMENSION {
            return Err(JsValue::from_str(&format!(
                "strict full-scale export is too large: {}x{} exceeds browser canvas limits",
                spec.width, spec.height
            )));
        }
        if pixel_count > MAX_EXPORT_PIXELS {
            return Err(JsValue::from_str(&format!(
                "strict full-scale export is too large: {}x{} would exceed {} pixels",
                spec.width, spec.height, MAX_EXPORT_PIXELS
            )));
        }

        let window = web_sys::window().ok_or_else(|| JsValue::from_str("window unavailable"))?;
        let document = window
            .document()
            .ok_or_else(|| JsValue::from_str("document unavailable"))?;
        let canvas = document
            .create_element("canvas")?
            .dyn_into::<HtmlCanvasElement>()?;
        canvas.set_width(spec.width);
        canvas.set_height(spec.height);

        let mut pixels = vec![0_u8; pixel_count * 4];
        fill_background(&mut pixels);

        for vertex in self.vertices.chunks_exact(5) {
            let center_x = vertex[0] as f64;
            let center_y = vertex[1] as f64;
            let color = [
                channel_to_u8(vertex[2]),
                channel_to_u8(vertex[3]),
                channel_to_u8(vertex[4]),
                255,
            ];
            draw_export_piece(&mut pixels, &spec, center_x, center_y, color);
        }

        let image = ImageData::new_with_u8_clamped_array_and_sh(
            Clamped(pixels.as_mut_slice()),
            spec.width,
            spec.height,
        )?;
        let context = canvas
            .get_context("2d")?
            .ok_or_else(|| JsValue::from_str("2d context unavailable"))?
            .dyn_into::<CanvasRenderingContext2d>()?;
        context.put_image_data(&image, 0.0, 0.0)?;
        Ok(canvas)
    }

    fn world_scale(&self, width: f64, height: f64) -> f64 {
        match self.settings.display_mode {
            DisplayMode::FitScreen => {
                let extent = self.fit_extent();
                width.min(height) / (2.0 * extent + 4.0)
            }
            DisplayMode::PixelOneToOne => self.settings.zoom.max(1) as f64,
        }
    }

    fn fit_extent(&self) -> f64 {
        match self.settings.board {
            BoardKind::LatticeTriangle => 3.0_f64.sqrt() * self.settings.radius.max(1.0),
            _ => self.settings.radius.max(1.0),
        }
    }

    fn render_state(&self, width: u32, height: u32) -> RenderState {
        let scale = self.world_scale(width as f64, height as f64);
        let radius_px = (scale * rendered_piece_radius(&self.settings)).max(1.0);
        let shape = shader_shape(&self.settings);

        RenderState {
            width,
            height,
            scale: scale as f32,
            point_size: (radius_px * 2.0) as f32,
            shape,
        }
    }

    fn export_spec(&self, resolution_scale: f64) -> ExportSpec {
        let radius = self.settings.radius.max(1.0);
        let board = self.settings.board;
        let square_pixel_cells =
            board == BoardKind::LatticeSquare && self.settings.shape == ShapeKind::Square;
        let base_scale = if square_pixel_cells { 1.0 } else { 4.0 };
        let scale = (base_scale * resolution_scale.clamp(0.25, 1.0)).max(0.25);
        let piece_radius = rendered_piece_radius(&self.settings).max(0.0);
        let margin = if square_pixel_cells {
            0.0
        } else {
            piece_radius
        };

        let (min_x, max_x, min_y, max_y) = match board {
            BoardKind::LatticeSquare | BoardKind::ContinuousArchimedean => (
                -radius - margin,
                radius + margin,
                -radius - margin,
                radius + margin,
            ),
            BoardKind::LatticeHex => {
                let hex_extent_x = 3.0_f64.sqrt() * radius;
                let hex_extent_y = 1.5 * radius;
                (
                    -hex_extent_x - margin,
                    hex_extent_x + margin,
                    -hex_extent_y - margin,
                    hex_extent_y + margin,
                )
            }
            BoardKind::LatticeTriangle => {
                let tri_extent_x = 1.5 * radius;
                let tri_extent_y = 3.0_f64.sqrt() * radius;
                (
                    -tri_extent_x - margin,
                    tri_extent_x + margin,
                    -tri_extent_y - margin,
                    tri_extent_y + margin,
                )
            }
        };

        let width = ((max_x - min_x) * scale).round() as u32 + 1;
        let height = ((max_y - min_y) * scale).round() as u32 + 1;
        let shape = export_shape(&self.settings);

        ExportSpec {
            min_x,
            max_y,
            scale,
            width: width.max(1),
            height: height.max(1),
            piece_radius,
            shape,
        }
    }

    fn clamp_pan_to_view(&mut self) {
        let scale = self.world_scale(self.canvas.width() as f64, self.canvas.height() as f64);
        if scale <= f64::EPSILON {
            self.pan_x = 0.0;
            self.pan_y = 0.0;
            return;
        }

        let extent = self.fit_extent() + rendered_piece_radius(&self.settings);
        let half_width = self.canvas.width() as f64 / (2.0 * scale);
        let half_height = self.canvas.height() as f64 / (2.0 * scale);
        let max_x = (extent - half_width).max(0.0);
        let max_y = (extent - half_height).max(0.0);
        self.pan_x = self.pan_x.clamp(-max_x, max_x);
        self.pan_y = self.pan_y.clamp(-max_y, max_y);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingUpload {
    None,
    Append { start_float: usize },
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct RenderState {
    width: u32,
    height: u32,
    scale: f32,
    point_size: f32,
    shape: i32,
}

#[derive(Clone, Copy, Debug)]
struct ExportSpec {
    min_x: f64,
    max_y: f64,
    scale: f64,
    width: u32,
    height: u32,
    piece_radius: f64,
    shape: ExportShape,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExportShape {
    Square,
    Circle,
    Hex,
    Triangle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TrackKey {
    board: BoardKind,
    radius_floor: i64,
    continuous_offset_bits: u64,
    enabled: bool,
}

impl TrackKey {
    fn from_settings(settings: &EngineSettings) -> Option<Self> {
        let enabled = settings.track_opacity > f32::EPSILON;
        if !enabled {
            return None;
        }

        Some(Self {
            board: settings.board,
            radius_floor: settings.radius.max(0.0).floor() as i64,
            continuous_offset_bits: settings.continuous_offset.to_bits(),
            enabled,
        })
    }
}

fn rendered_piece_radius(settings: &EngineSettings) -> f64 {
    if settings.shape == ShapeKind::Hex && settings.board == BoardKind::LatticeHex {
        settings.piece_radius * 2.0
    } else if settings.shape == ShapeKind::Triangle && settings.board == BoardKind::LatticeTriangle
    {
        settings.piece_radius * (2.0 / 3.0_f64.sqrt())
    } else {
        settings.piece_radius
    }
}

fn shader_shape(settings: &EngineSettings) -> i32 {
    if settings.board == BoardKind::ContinuousArchimedean || settings.shape == ShapeKind::Circle {
        1
    } else if settings.shape == ShapeKind::Hex {
        2
    } else if settings.shape == ShapeKind::Triangle {
        3
    } else {
        0
    }
}

fn export_shape(settings: &EngineSettings) -> ExportShape {
    if settings.board == BoardKind::ContinuousArchimedean || settings.shape == ShapeKind::Circle {
        ExportShape::Circle
    } else if settings.shape == ShapeKind::Hex {
        ExportShape::Hex
    } else if settings.shape == ShapeKind::Triangle {
        ExportShape::Triangle
    } else {
        ExportShape::Square
    }
}

fn build_track_vertices(settings: &EngineSettings) -> Vec<f32> {
    if settings.track_opacity <= f32::EPSILON {
        return Vec::new();
    }

    let mut vertices = Vec::new();
    match settings.board {
        BoardKind::LatticeSquare => {
            let bound = settings.radius.max(0.0).floor() as i64;
            for coord in SquareSpiral::new() {
                if coord.x.abs().max(coord.y.abs()) > bound {
                    break;
                }
                if vertices.len() / FLOATS_PER_VERTEX >= MAX_TRACK_POINTS {
                    break;
                }
                let point = coord.to_point();
                push_track_vertex(&mut vertices, point.x, point.y);
            }
        }
        BoardKind::LatticeHex => {
            let bound = settings.radius.max(0.0).floor() as i64;
            for coord in HexSpiral::new() {
                let (x, y, z) = coord.cube();
                if x.abs().max(y.abs()).max(z.abs()) > bound {
                    break;
                }
                if vertices.len() / FLOATS_PER_VERTEX >= MAX_TRACK_POINTS {
                    break;
                }
                let point = coord.to_point();
                push_track_vertex(&mut vertices, point.x, point.y);
            }
        }
        BoardKind::LatticeTriangle => {
            let bound = settings.radius.max(0.0).floor() as i64;
            for (index, coord) in TriangleSpiral::new().enumerate() {
                if TriangleSpiral::radius_for_index(index as u64) > bound as u64 {
                    break;
                }
                if vertices.len() / FLOATS_PER_VERTEX >= MAX_TRACK_POINTS {
                    break;
                }
                let point = coord.to_point();
                push_track_vertex(&mut vertices, point.x, point.y);
            }
        }
        BoardKind::ContinuousArchimedean => {
            let radius = settings.radius.max(0.0);
            let start_theta =
                ArchimedeanSpiral::theta_for_arc_length_from_origin(settings.continuous_offset)
                    .unwrap_or(0.0);
            let end_theta = radius * std::f64::consts::TAU;
            let step = ((end_theta - start_theta).max(0.0) / MAX_TRACK_POINTS as f64).max(0.05);
            let mut theta = start_theta.min(end_theta);
            while theta <= end_theta {
                let point = ArchimedeanSpiral::position(theta);
                push_track_vertex(&mut vertices, point.x, point.y);
                theta += step;
            }
        }
    }

    vertices
}

fn push_track_vertex(vertices: &mut Vec<f32>, x: f64, y: f64) {
    vertices.extend_from_slice(&[x as f32, y as f32, 0.70, 0.78, 0.86]);
}

fn fill_background(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[8, 9, 10, 255]);
    }
}

fn draw_export_piece(
    pixels: &mut [u8],
    spec: &ExportSpec,
    center_x: f64,
    center_y: f64,
    color: [u8; 4],
) {
    let center_px = ((center_x - spec.min_x) * spec.scale).round() as i32;
    let center_py = ((spec.max_y - center_y) * spec.scale).round() as i32;
    let reach = (spec.piece_radius * spec.scale).ceil().max(0.0) as i32;
    let reach = reach.max(0);

    for py in (center_py - reach)..=(center_py + reach) {
        for px in (center_px - reach)..=(center_px + reach) {
            if px < 0 || py < 0 || px >= spec.width as i32 || py >= spec.height as i32 {
                continue;
            }

            let world_x = spec.min_x + px as f64 / spec.scale;
            let world_y = spec.max_y - py as f64 / spec.scale;
            if export_shape_contains(
                spec.shape,
                center_x,
                center_y,
                world_x,
                world_y,
                spec.piece_radius,
            ) {
                set_pixel(pixels, spec.width, px as u32, py as u32, color);
            }
        }
    }
}

fn export_shape_contains(
    shape: ExportShape,
    center_x: f64,
    center_y: f64,
    world_x: f64,
    world_y: f64,
    piece_radius: f64,
) -> bool {
    let epsilon = 1.0e-9;
    match shape {
        ExportShape::Square => {
            (world_x - center_x).abs() <= piece_radius + epsilon
                && (world_y - center_y).abs() <= piece_radius + epsilon
        }
        ExportShape::Circle => {
            (world_x - center_x).hypot(world_y - center_y) <= piece_radius + epsilon
        }
        ExportShape::Hex => {
            let dx = ((world_x - center_x) / piece_radius.max(epsilon)).abs();
            let dy = ((world_y - center_y) / piece_radius.max(epsilon)).abs();
            dx <= 0.866_025_403_784 + epsilon
                && dy <= 1.0 + epsilon
                && dx / 3.0_f64.sqrt() + dy <= 1.0 + epsilon
        }
        ExportShape::Triangle => {
            let px = (world_x - center_x) / piece_radius.max(epsilon);
            let py = (world_y - center_y) / piece_radius.max(epsilon);
            py >= -0.5 - epsilon
                && py <= 1.0 + epsilon
                && px.abs() <= (1.0 - py) / 3.0_f64.sqrt() + epsilon
        }
    }
}

fn set_pixel(pixels: &mut [u8], width: u32, x: u32, y: u32, color: [u8; 4]) {
    let index = ((y * width + x) * 4) as usize;
    if let Some(pixel) = pixels.get_mut(index..index + 4) {
        pixel.copy_from_slice(&color);
    }
}

fn channel_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn compile_shader(gl: &Gl, shader_type: u32, source: &str) -> Result<WebGlShader, JsValue> {
    let shader = gl
        .create_shader(shader_type)
        .ok_or_else(|| JsValue::from_str("failed to create WebGL shader"))?;
    gl.shader_source(&shader, source);
    gl.compile_shader(&shader);

    if gl
        .get_shader_parameter(&shader, Gl::COMPILE_STATUS)
        .as_bool()
        .unwrap_or(false)
    {
        Ok(shader)
    } else {
        Err(JsValue::from_str(
            &gl.get_shader_info_log(&shader)
                .unwrap_or_else(|| "unknown WebGL shader compile error".to_string()),
        ))
    }
}

fn link_program(
    gl: &Gl,
    vertex_shader: &WebGlShader,
    fragment_shader: &WebGlShader,
) -> Result<WebGlProgram, JsValue> {
    let program = gl
        .create_program()
        .ok_or_else(|| JsValue::from_str("failed to create WebGL program"))?;
    gl.attach_shader(&program, vertex_shader);
    gl.attach_shader(&program, fragment_shader);
    gl.link_program(&program);

    if gl
        .get_program_parameter(&program, Gl::LINK_STATUS)
        .as_bool()
        .unwrap_or(false)
    {
        Ok(program)
    } else {
        Err(JsValue::from_str(
            &gl.get_program_info_log(&program)
                .unwrap_or_else(|| "unknown WebGL program link error".to_string()),
        ))
    }
}

fn attrib_location(gl: &Gl, program: &WebGlProgram, name: &str) -> Result<u32, JsValue> {
    let location = gl.get_attrib_location(program, name);
    if location < 0 {
        Err(JsValue::from_str(&format!(
            "missing WebGL attribute {name}"
        )))
    } else {
        Ok(location as u32)
    }
}

fn uniform_location(
    gl: &Gl,
    program: &WebGlProgram,
    name: &str,
) -> Result<WebGlUniformLocation, JsValue> {
    gl.get_uniform_location(program, name)
        .ok_or_else(|| JsValue::from_str(&format!("missing WebGL uniform {name}")))
}

#[allow(dead_code)]
fn _board_name(board: BoardKind) -> &'static str {
    match board {
        BoardKind::LatticeSquare => "LatticeSquare",
        BoardKind::LatticeHex => "LatticeHex",
        BoardKind::LatticeTriangle => "LatticeTriangle",
        BoardKind::ContinuousArchimedean => "ContinuousArchimedean",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_triangle_render_radius_touches_without_overlap() {
        let settings = EngineSettings {
            board: BoardKind::LatticeTriangle,
            shape: ShapeKind::Triangle,
            piece_radius: 0.5,
            ..EngineSettings::default()
        };

        assert!((rendered_piece_radius(&settings) - 1.0 / 3.0_f64.sqrt()).abs() < 1.0e-12);
    }
}
