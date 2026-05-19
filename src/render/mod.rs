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
uniform float u_saturation;

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
        vec2 p = 1.0 - gl_PointCoord * 2.0;
        if (p.y < -0.5 || p.y > 1.0 || abs(p.x) > (1.0 - p.y) * 0.5773503) {
            discard;
        }
    }

    float luminance = dot(v_color, vec3(0.2126, 0.7152, 0.0722));
    vec3 color = mix(vec3(luminance), v_color, clamp(u_saturation, 0.0, 1.0));
    gl_FragColor = vec4(color, u_alpha);
}
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportKind {
    FullPng,
    Png,
    JpegHalf,
}

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
    saturation_uniform: WebGlUniformLocation,
    vertices: Vec<f32>,
    track_vertices: Vec<f32>,
    pending_upload: PendingUpload,
    track_upload_pending: bool,
    gpu_capacity_floats: usize,
    track_buffer: WebGlBuffer,
    settings: EngineSettings,
    color_state: ColorState,
    color_saturation: f32,
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
        let saturation_uniform = uniform_location(&gl, &program, "u_saturation")?;
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
            saturation_uniform,
            vertices: Vec::new(),
            track_vertices: Vec::new(),
            pending_upload: PendingUpload::Full,
            track_upload_pending: true,
            gpu_capacity_floats: 0,
            track_buffer,
            settings: EngineSettings::default(),
            color_state: ColorState::default(),
            color_saturation: 1.0,
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

    pub fn set_color_saturation(&mut self, color_saturation: f32) -> Result<(), JsValue> {
        self.color_saturation = color_saturation.clamp(0.0, 1.0);
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
        if self.settings.display_mode != DisplayMode::PixelOneToOne {
            return Ok(());
        }

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

        let old_zoom = self.settings.zoom.clamp(1, 32);
        let new_zoom = (old_zoom as i32 + delta).clamp(1, 32) as u8;
        if new_zoom == old_zoom {
            return Ok(old_zoom);
        }

        let window = web_sys::window().ok_or_else(|| JsValue::from_str("window unavailable"))?;
        let dpr = window.device_pixel_ratio();
        let px = client_x * dpr;
        let py = client_y * dpr;
        let width = self.canvas.width() as f64;
        let height = self.canvas.height() as f64;
        let bounds = self.view_bounds(rendered_piece_radius(&self.settings));
        let old_scale = self.world_scale(width, height);
        let world_x = (px - width * 0.5) / old_scale - (self.pan_x - bounds.center_x());
        let world_y = (height * 0.5 - py) / old_scale - (self.pan_y - bounds.center_y());

        self.settings.zoom = new_zoom;
        let new_scale = self.world_scale(width, height);
        self.pan_x = (px - width * 0.5) / new_scale + bounds.center_x() - world_x;
        self.pan_y = (height * 0.5 - py) / new_scale + bounds.center_y() - world_y;
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
            (self.pan_x - render_state.center_x) as f32,
            (self.pan_y - render_state.center_y) as f32,
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
        self.gl.uniform1f(
            Some(&self.saturation_uniform),
            self.color_saturation.clamp(0.0, 1.0),
        );
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
        self.gl.uniform1f(Some(&self.saturation_uniform), 1.0);
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
        kind: ExportKind,
        encoder_quality: Option<f64>,
    ) -> Result<(), JsValue> {
        let export_canvas = self.pixel_export_canvas(kind)?;
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

    fn pixel_export_canvas(&self, kind: ExportKind) -> Result<HtmlCanvasElement, JsValue> {
        let spec = self.export_spec(kind, 1.0);
        if spec.width > MAX_EXPORT_DIMENSION || spec.height > MAX_EXPORT_DIMENSION {
            return Err(JsValue::from_str(&format!(
                "strict full-scale export is too large: {}x{} exceeds browser canvas limits",
                spec.width, spec.height
            )));
        }
        let pixel_count = checked_pixel_count(spec.width, spec.height)?;
        if pixel_count > MAX_EXPORT_PIXELS {
            return Err(JsValue::from_str(&format!(
                "strict full-scale export is too large: {}x{} would exceed {} pixels",
                spec.width, spec.height, MAX_EXPORT_PIXELS
            )));
        }

        if !spec.square_pixel_cells {
            let supersampled = self.export_spec(kind, 2.0);
            if supersampled.width > MAX_EXPORT_DIMENSION
                || supersampled.height > MAX_EXPORT_DIMENSION
            {
                return Err(JsValue::from_str(&format!(
                    "strict smoothed export is too large: {}x{} supersample would exceed browser limits",
                    supersampled.width, supersampled.height
                )));
            }
            let supersampled_pixel_count =
                checked_pixel_count(supersampled.width, supersampled.height)?;
            if supersampled_pixel_count > MAX_EXPORT_PIXELS {
                return Err(JsValue::from_str(&format!(
                    "strict smoothed export is too large: {}x{} supersample would exceed browser limits",
                    supersampled.width, supersampled.height
                )));
            }

            let mut high_pixels = vec![0_u8; checked_rgba_len(supersampled_pixel_count)?];
            fill_background(&mut high_pixels);
            for vertex in self.vertices.chunks_exact(5) {
                let center_x = vertex[0] as f64;
                let center_y = vertex[1] as f64;
                let color = [
                    channel_to_u8(vertex[2]),
                    channel_to_u8(vertex[3]),
                    channel_to_u8(vertex[4]),
                    255,
                ];
                draw_export_piece(&mut high_pixels, &supersampled, center_x, center_y, color);
            }

            let mut pixels = vec![0_u8; checked_rgba_len(pixel_count)?];
            downsample_2x(
                &high_pixels,
                supersampled.width,
                supersampled.height,
                &mut pixels,
                spec.width,
                spec.height,
            );
            return canvas_from_pixels(spec.width, spec.height, pixels);
        }

        let mut pixels = vec![0_u8; checked_rgba_len(pixel_count)?];
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

        canvas_from_pixels(spec.width, spec.height, pixels)
    }

    fn world_scale(&self, width: f64, height: f64) -> f64 {
        world_scale_for_settings(&self.settings, width, height)
    }

    fn view_bounds(&self, margin: f64) -> WorldBounds {
        board_world_bounds(&self.settings, margin)
    }

    fn render_state(&self, width: u32, height: u32) -> RenderState {
        let scale = self.world_scale(width as f64, height as f64);
        let radius_px = (scale * rendered_piece_radius(&self.settings)).max(1.0);
        let shape = shader_shape(&self.settings);
        let bounds = self.view_bounds(rendered_piece_radius(&self.settings));

        RenderState {
            width,
            height,
            scale: scale as f32,
            point_size: (radius_px * 2.0) as f32,
            shape,
            center_x: bounds.center_x(),
            center_y: bounds.center_y(),
        }
    }

    fn export_spec(&self, kind: ExportKind, supersample: f64) -> ExportSpec {
        let board = self.settings.board;
        let square_pixel_cells =
            board == BoardKind::LatticeSquare && self.settings.shape == ShapeKind::Square;
        let piece_radius = rendered_piece_radius(&self.settings).max(0.0);
        let scale = export_scale(kind, square_pixel_cells, piece_radius) * supersample.max(1.0);
        let margin = if square_pixel_cells {
            0.0
        } else {
            piece_radius
        };

        let bounds = board_world_bounds(&self.settings, margin);
        let (min_x, max_x, min_y, max_y) = (bounds.min_x, bounds.max_x, bounds.min_y, bounds.max_y);

        let width = export_dimension(max_x - min_x, scale);
        let height = export_dimension(max_y - min_y, scale);
        let shape = export_shape(&self.settings);

        ExportSpec {
            min_x,
            max_y,
            scale,
            width: width.max(1),
            height: height.max(1),
            piece_radius,
            shape,
            square_pixel_cells,
        }
    }

    fn clamp_pan_to_view(&mut self) {
        let scale = self.world_scale(self.canvas.width() as f64, self.canvas.height() as f64);
        if scale <= f64::EPSILON {
            self.pan_x = 0.0;
            self.pan_y = 0.0;
            return;
        }

        if self.settings.display_mode != DisplayMode::PixelOneToOne {
            self.pan_x = 0.0;
            self.pan_y = 0.0;
            return;
        }

        let bounds = self.view_bounds(rendered_piece_radius(&self.settings));
        let half_board_width = bounds.width() * 0.5;
        let half_board_height = bounds.height() * 0.5;
        let half_width = self.canvas.width() as f64 / (2.0 * scale);
        let half_height = self.canvas.height() as f64 / (2.0 * scale);
        let edge_room =
            half_width.min(half_height) * 0.25 + rendered_piece_radius(&self.settings) + 4.0;
        let max_x = (half_board_width + edge_room - half_width).max(0.0);
        let max_y = (half_board_height + edge_room - half_height).max(0.0);
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
    center_x: f64,
    center_y: f64,
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
    square_pixel_cells: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct WorldBounds {
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
}

impl WorldBounds {
    #[must_use]
    fn width(self) -> f64 {
        (self.max_x - self.min_x).max(1.0)
    }

    #[must_use]
    fn height(self) -> f64 {
        (self.max_y - self.min_y).max(1.0)
    }

    #[must_use]
    fn center_x(self) -> f64 {
        0.5 * (self.min_x + self.max_x)
    }

    #[must_use]
    fn center_y(self) -> f64 {
        0.5 * (self.min_y + self.max_y)
    }
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
    radius_key: u64,
    continuous_offset_bits: u64,
}

impl TrackKey {
    fn from_settings(settings: &EngineSettings) -> Option<Self> {
        let enabled = settings.track_opacity > f32::EPSILON;
        if !enabled {
            return None;
        }

        Some(Self {
            board: settings.board,
            radius_key: match settings.board {
                BoardKind::ContinuousArchimedean => settings.radius.max(0.0).to_bits(),
                _ => settings.radius.max(0.0).floor().to_bits(),
            },
            continuous_offset_bits: settings.continuous_offset.to_bits(),
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

fn export_scale(kind: ExportKind, square_pixel_cells: bool, piece_radius: f64) -> f64 {
    let full_scale: f64 = if square_pixel_cells { 1.0 } else { 4.0 };
    match kind {
        ExportKind::FullPng => full_scale,
        ExportKind::Png if square_pixel_cells => full_scale,
        ExportKind::Png => full_scale.min(1.0 / piece_radius.max(0.125)),
        ExportKind::JpegHalf => full_scale * 0.5,
    }
}

fn board_world_bounds(settings: &EngineSettings, margin: f64) -> WorldBounds {
    let radius = settings.radius.max(1.0);
    let margin = margin.max(0.0);

    match settings.board {
        BoardKind::LatticeSquare | BoardKind::ContinuousArchimedean => WorldBounds {
            min_x: -radius - margin,
            max_x: radius + margin,
            min_y: -radius - margin,
            max_y: radius + margin,
        },
        BoardKind::LatticeHex => {
            let hex_extent_x = 3.0_f64.sqrt() * radius;
            let hex_extent_y = 1.5 * radius;
            WorldBounds {
                min_x: -hex_extent_x - margin,
                max_x: hex_extent_x + margin,
                min_y: -hex_extent_y - margin,
                max_y: hex_extent_y + margin,
            }
        }
        BoardKind::LatticeTriangle => {
            let shell = settings.radius.max(0.0).floor().max(1.0);
            WorldBounds {
                min_x: -1.5 * shell - margin,
                max_x: 1.5 * shell - 0.5 + margin,
                min_y: -0.5 * 3.0_f64.sqrt() * shell - margin,
                max_y: 3.0_f64.sqrt() * shell + margin,
            }
        }
    }
}

fn world_scale_for_settings(settings: &EngineSettings, width: f64, height: f64) -> f64 {
    let fit_scale = fit_screen_scale(
        width,
        height,
        board_world_bounds(settings, rendered_piece_radius(settings)),
    );
    match settings.display_mode {
        DisplayMode::FitScreen => fit_scale,
        DisplayMode::PixelOneToOne => fit_scale * settings.zoom.clamp(1, 32) as f64,
    }
}

fn fit_screen_scale(width: f64, height: f64, bounds: WorldBounds) -> f64 {
    (width / (bounds.width() + 4.0)).min(height / (bounds.height() + 4.0))
}

fn build_track_vertices(settings: &EngineSettings) -> Vec<f32> {
    if settings.track_opacity <= f32::EPSILON {
        return Vec::new();
    }

    let mut vertices = Vec::new();
    match settings.board {
        BoardKind::LatticeSquare => {
            let bound = settings.radius.max(0.0).floor() as u64;
            let side = bound.saturating_mul(2).saturating_add(1);
            let total_points = side.saturating_mul(side);
            push_lattice_track_points(&mut vertices, total_points, |index| {
                let point = SquareSpiral::coord_at_index(index).to_point();
                (point.x, point.y)
            });
        }
        BoardKind::LatticeHex => {
            let bound = settings.radius.max(0.0).floor() as u64;
            let total_points = 1_u64.saturating_add(
                3_u64
                    .saturating_mul(bound)
                    .saturating_mul(bound.saturating_add(1)),
            );
            push_lattice_track_points(&mut vertices, total_points, |index| {
                let point = HexSpiral::coord_at_index(index).to_point();
                (point.x, point.y)
            });
        }
        BoardKind::LatticeTriangle => {
            let bound = settings.radius.max(0.0).floor() as u64;
            let total_points = triangular_number(bound.saturating_mul(3)).saturating_add(1);
            push_lattice_track_points(&mut vertices, total_points, |index| {
                let point = TriangleSpiral::coord_at_index(index).to_point();
                (point.x, point.y)
            });
        }
        BoardKind::ContinuousArchimedean => {
            let radius = settings.radius.max(0.0);
            let start_theta =
                ArchimedeanSpiral::theta_for_arc_length_from_origin(settings.continuous_offset)
                    .unwrap_or(0.0);
            let end_theta = radius * std::f64::consts::TAU;
            let step = ((end_theta - start_theta).max(0.0) / MAX_TRACK_POINTS as f64).max(0.05);
            let mut theta = start_theta.min(end_theta);
            let mut last_theta = None;
            while theta <= end_theta {
                let point = ArchimedeanSpiral::position(theta);
                push_track_vertex(&mut vertices, point.x, point.y);
                last_theta = Some(theta);
                theta += step;
            }
            if last_theta.is_none_or(|theta| theta < end_theta) {
                let point = ArchimedeanSpiral::position(end_theta);
                push_track_vertex(&mut vertices, point.x, point.y);
            }
        }
    }

    vertices
}

fn push_lattice_track_points<F>(vertices: &mut Vec<f32>, total_points: u64, mut point_at: F)
where
    F: FnMut(u64) -> (f64, f64),
{
    if total_points == 0 {
        return;
    }

    if let Ok(points) = usize::try_from(total_points) {
        vertices.reserve(points.saturating_mul(FLOATS_PER_VERTEX));
    }
    for index in 0..total_points {
        let point = point_at(index);
        push_track_vertex(vertices, point.0, point.1);
    }
}

#[must_use]
fn triangular_number(n: u64) -> u64 {
    n.saturating_mul(n.saturating_add(1)) / 2
}

fn export_dimension(span: f64, scale: f64) -> u32 {
    let pixels = span.max(0.0) * scale.max(0.0);
    if !pixels.is_finite() || pixels >= u32::MAX as f64 {
        return u32::MAX;
    }
    (pixels.round() as u32).saturating_add(1).max(1)
}

fn checked_pixel_count(width: u32, height: u32) -> Result<usize, JsValue> {
    (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| JsValue::from_str("export dimensions are too large"))
}

fn checked_rgba_len(pixel_count: usize) -> Result<usize, JsValue> {
    pixel_count
        .checked_mul(4)
        .ok_or_else(|| JsValue::from_str("export pixel buffer is too large"))
}

fn push_track_vertex(vertices: &mut Vec<f32>, x: f64, y: f64) {
    vertices.extend_from_slice(&[x as f32, y as f32, 0.70, 0.78, 0.86]);
}

fn fill_background(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[8, 9, 10, 255]);
    }
}

fn downsample_2x(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    target: &mut [u8],
    target_width: u32,
    target_height: u32,
) {
    for y in 0..target_height {
        for x in 0..target_width {
            let mut accum = [0_u32; 4];
            let mut count = 0_u32;
            for sy in (y * 2)..=((y * 2 + 1).min(source_height.saturating_sub(1))) {
                for sx in (x * 2)..=((x * 2 + 1).min(source_width.saturating_sub(1))) {
                    let source_index = ((sy * source_width + sx) * 4) as usize;
                    if let Some(pixel) = source.get(source_index..source_index + 4) {
                        for channel in 0..4 {
                            accum[channel] += pixel[channel] as u32;
                        }
                        count += 1;
                    }
                }
            }

            let target_index = ((y * target_width + x) * 4) as usize;
            if let Some(pixel) = target.get_mut(target_index..target_index + 4) {
                let count = count.max(1);
                for channel in 0..4 {
                    pixel[channel] = (accum[channel] / count) as u8;
                }
            }
        }
    }
}

fn canvas_from_pixels(
    width: u32,
    height: u32,
    mut pixels: Vec<u8>,
) -> Result<HtmlCanvasElement, JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("window unavailable"))?;
    let document = window
        .document()
        .ok_or_else(|| JsValue::from_str("document unavailable"))?;
    let canvas = document
        .create_element("canvas")?
        .dyn_into::<HtmlCanvasElement>()?;
    canvas.set_width(width);
    canvas.set_height(height);
    let image =
        ImageData::new_with_u8_clamped_array_and_sh(Clamped(pixels.as_mut_slice()), width, height)?;
    let context = canvas
        .get_context("2d")?
        .ok_or_else(|| JsValue::from_str("2d context unavailable"))?
        .dyn_into::<CanvasRenderingContext2d>()?;
    context.put_image_data(&image, 0.0, 0.0)?;
    Ok(canvas)
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
            let px = -(world_x - center_x) / piece_radius.max(epsilon);
            let py = -(world_y - center_y) / piece_radius.max(epsilon);
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

    #[test]
    fn triangle_export_shape_is_flipped_downward() {
        assert!(export_shape_contains(
            ExportShape::Triangle,
            0.0,
            0.0,
            0.0,
            -0.9,
            1.0
        ));
        assert!(!export_shape_contains(
            ExportShape::Triangle,
            0.0,
            0.0,
            0.0,
            0.9,
            1.0
        ));
    }

    #[test]
    fn export_scales_keep_square_nearest_and_cap_regular_png() {
        assert_eq!(export_scale(ExportKind::FullPng, true, 0.5), 1.0);
        assert_eq!(export_scale(ExportKind::Png, true, 0.5), 1.0);
        assert_eq!(export_scale(ExportKind::JpegHalf, true, 0.5), 0.5);
        assert_eq!(export_scale(ExportKind::FullPng, false, 0.5), 4.0);
        assert_eq!(export_scale(ExportKind::Png, false, 0.5), 2.0);
    }

    #[test]
    fn lattice_track_line_strip_reaches_requested_radius() {
        for board in [
            BoardKind::LatticeSquare,
            BoardKind::LatticeHex,
            BoardKind::LatticeTriangle,
        ] {
            let settings = EngineSettings {
                board,
                radius: 300.0,
                track_opacity: 0.5,
                ..EngineSettings::default()
            };
            let vertices = build_track_vertices(&settings);
            let last = &vertices[vertices.len() - FLOATS_PER_VERTEX..];
            let distance = match board {
                BoardKind::LatticeSquare => last[0].abs().max(last[1].abs()) as f64,
                BoardKind::LatticeHex | BoardKind::LatticeTriangle => {
                    (last[0] as f64).hypot(last[1] as f64)
                }
                BoardKind::ContinuousArchimedean => unreachable!(),
            };

            assert!(distance > 280.0, "board={board:?}, distance={distance}");
        }
    }

    #[test]
    fn lattice_track_radius_150_draws_every_adjacent_segment() {
        for board in [
            BoardKind::LatticeSquare,
            BoardKind::LatticeHex,
            BoardKind::LatticeTriangle,
        ] {
            let settings = EngineSettings {
                board,
                radius: 150.0,
                track_opacity: 0.5,
                ..EngineSettings::default()
            };
            let vertices = build_track_vertices(&settings);
            let expected_points = match board {
                BoardKind::LatticeSquare => {
                    let side = 150_u64 * 2 + 1;
                    side * side
                }
                BoardKind::LatticeHex => 1 + 3 * 150_u64 * 151,
                BoardKind::LatticeTriangle => triangular_number(150_u64 * 3) + 1,
                BoardKind::ContinuousArchimedean => unreachable!(),
            };

            assert_eq!(
                vertices.len() / FLOATS_PER_VERTEX,
                expected_points as usize,
                "board={board:?}"
            );
        }
    }

    #[test]
    fn lattice_track_radius_300_draws_connected_adjacent_path() {
        for board in [
            BoardKind::LatticeSquare,
            BoardKind::LatticeHex,
            BoardKind::LatticeTriangle,
        ] {
            let settings = EngineSettings {
                board,
                radius: 300.0,
                track_opacity: 0.5,
                ..EngineSettings::default()
            };
            let vertices = build_track_vertices(&settings);
            let point_count = vertices.len() / FLOATS_PER_VERTEX;
            assert!(
                point_count > 250_000,
                "board={board:?}, points={point_count}"
            );

            let mut previous: Option<(f64, f64)> = None;
            let mut max_segment = 0.0_f64;
            for point in vertices.chunks_exact(FLOATS_PER_VERTEX) {
                let current = (point[0] as f64, point[1] as f64);
                if let Some((x0, y0)) = previous {
                    max_segment = max_segment.max((current.0 - x0).hypot(current.1 - y0));
                }
                previous = Some(current);
            }

            assert!(
                max_segment <= 2.0,
                "board={board:?}, max segment length={max_segment}"
            );
        }
    }

    #[test]
    fn continuous_track_cache_key_uses_exact_radius() {
        let small = EngineSettings {
            board: BoardKind::ContinuousArchimedean,
            radius: 240.1,
            track_opacity: 0.5,
            ..EngineSettings::default()
        };
        let mut large = small.clone();
        large.radius = 240.9;

        let small_key = TrackKey::from_settings(&small);
        let large_key = TrackKey::from_settings(&large);
        assert_ne!(small_key, large_key);
    }

    #[test]
    fn continuous_track_reaches_fractional_requested_radius() {
        let settings = EngineSettings {
            board: BoardKind::ContinuousArchimedean,
            radius: 240.75,
            track_opacity: 0.5,
            ..EngineSettings::default()
        };
        let vertices = build_track_vertices(&settings);
        let last = &vertices[vertices.len() - FLOATS_PER_VERTEX..];
        let distance = (last[0] as f64).hypot(last[1] as f64);

        assert!((distance - settings.radius).abs() < 1.0e-3);
    }

    #[test]
    fn triangle_world_bounds_follow_asymmetric_triangle_shell() {
        let settings = EngineSettings {
            board: BoardKind::LatticeTriangle,
            radius: 10.0,
            ..EngineSettings::default()
        };
        let bounds = board_world_bounds(&settings, 0.0);

        assert!(bounds.center_y() > 0.0);
        assert!(bounds.min_y > -3.0_f64.sqrt() * 10.0);
        assert_eq!(bounds.max_x, 14.5);
    }

    #[test]
    fn pixel_one_to_one_zoom_one_fits_requested_board_bounds() {
        let fit_settings = EngineSettings {
            board: BoardKind::LatticeTriangle,
            radius: 850.0,
            display_mode: DisplayMode::FitScreen,
            zoom: 1,
            ..EngineSettings::default()
        };
        let pixel_settings = EngineSettings {
            display_mode: DisplayMode::PixelOneToOne,
            ..fit_settings.clone()
        };

        let width = 1280.0;
        let height = 720.0;
        let fit_scale = world_scale_for_settings(&fit_settings, width, height);
        let pixel_scale = world_scale_for_settings(&pixel_settings, width, height);

        assert!((pixel_scale - fit_scale).abs() <= f64::EPSILON);
    }

    #[test]
    fn pixel_one_to_one_zoom_levels_scale_from_fit_bounds() {
        let settings = EngineSettings {
            board: BoardKind::LatticeSquare,
            radius: 100.0,
            display_mode: DisplayMode::PixelOneToOne,
            zoom: 4,
            ..EngineSettings::default()
        };
        let base = world_scale_for_settings(
            &EngineSettings {
                zoom: 1,
                ..settings.clone()
            },
            1200.0,
            800.0,
        );
        let zoomed = world_scale_for_settings(&settings, 1200.0, 800.0);

        assert!((zoomed - base * 4.0).abs() <= f64::EPSILON * 4.0);
    }
}
