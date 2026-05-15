use wasm_bindgen::prelude::*;
use wasm_bindgen::{Clamped, JsCast};
use web_sys::{
    CanvasRenderingContext2d, HtmlAnchorElement, HtmlCanvasElement, ImageData, WebGlBuffer,
    WebGlProgram, WebGlRenderingContext as Gl, WebGlShader, WebGlUniformLocation,
};

use crate::protocol::{
    BoardKind, ColorState, DisplayMode, EngineSettings, ShapeKind, VertexBufferUpdate,
};

const FLOATS_PER_VERTEX: usize = 5;
const BYTES_PER_FLOAT: usize = std::mem::size_of::<f32>();
const INITIAL_VERTEX_CAPACITY: usize = 16_384;

const VERTEX_SHADER: &str = r#"
attribute vec2 a_position;
attribute vec3 a_color;

uniform vec2 u_resolution;
uniform float u_scale;
uniform float u_point_size;

varying vec3 v_color;

void main() {
    vec2 screen = vec2(
        u_resolution.x * 0.5 + a_position.x * u_scale,
        u_resolution.y * 0.5 - a_position.y * u_scale
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

varying vec3 v_color;

void main() {
    if (u_shape == 1) {
        vec2 p = gl_PointCoord * 2.0 - 1.0;
        if (dot(p, p) > 1.0) {
            discard;
        }
    }

    gl_FragColor = vec4(v_color, 1.0);
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
    shape_uniform: WebGlUniformLocation,
    vertices: Vec<f32>,
    pending_upload: PendingUpload,
    gpu_capacity_floats: usize,
    settings: EngineSettings,
    color_state: ColorState,
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
        let shape_uniform = uniform_location(&gl, &program, "u_shape")?;

        gl.clear_color(0.031, 0.035, 0.039, 1.0);
        gl.disable(Gl::DEPTH_TEST);

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
            shape_uniform,
            vertices: Vec::new(),
            pending_upload: PendingUpload::Full,
            gpu_capacity_floats: 0,
            settings: EngineSettings::default(),
            color_state: ColorState::default(),
        };
        renderer.resize_to_viewport()?;
        Ok(renderer)
    }

    pub fn set_settings(&mut self, settings: EngineSettings) -> Result<(), JsValue> {
        self.settings = settings;
        self.redraw()
    }

    pub fn set_color_state(&mut self, color_state: ColorState) -> Result<(), JsValue> {
        self.color_state = color_state;
        self.redraw()
    }

    pub fn clear_placements(&mut self) -> Result<(), JsValue> {
        self.vertices.clear();
        self.pending_upload = PendingUpload::Full;
        self.redraw()
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

        if self.vertices.is_empty() {
            self.gl.clear(Gl::COLOR_BUFFER_BIT);
            self.pending_upload = PendingUpload::None;
            return Ok(());
        }

        self.gl.use_program(Some(&self.program));
        self.gl.bind_buffer(Gl::ARRAY_BUFFER, Some(&self.buffer));
        self.sync_gpu_buffer()?;

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

        self.gl
            .uniform2f(Some(&self.resolution_uniform), width as f32, height as f32);
        self.gl
            .uniform1f(Some(&self.scale_uniform), render_state.scale);
        self.gl
            .uniform1f(Some(&self.point_size_uniform), render_state.point_size);
        self.gl
            .uniform1i(Some(&self.shape_uniform), render_state.shape);

        self.gl.clear(Gl::COLOR_BUFFER_BIT);
        self.gl.draw_arrays(
            Gl::POINTS,
            0,
            (self.vertices.len() / FLOATS_PER_VERTEX) as i32,
        );

        Ok(())
    }

    pub fn download_image(&self, mime_type: &str, filename: &str) -> Result<(), JsValue> {
        let export_canvas = self.pixel_export_canvas()?;
        let data_url = export_canvas.to_data_url_with_type(mime_type)?;
        let window = web_sys::window().ok_or_else(|| JsValue::from_str("window unavailable"))?;
        let document = window
            .document()
            .ok_or_else(|| JsValue::from_str("document unavailable"))?;
        let anchor = document
            .create_element("a")?
            .dyn_into::<HtmlAnchorElement>()?;

        anchor.set_href(&data_url);
        anchor.set_download(filename);
        anchor.click();
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

    fn pixel_export_canvas(&self) -> Result<HtmlCanvasElement, JsValue> {
        let spec = self.export_spec();
        let pixel_count = spec.width as usize * spec.height as usize;
        if pixel_count > 64_000_000 {
            return Err(JsValue::from_str(
                "pixel-perfect export would exceed 64 million pixels",
            ));
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
        self.settings.radius.max(1.0)
    }

    fn render_state(&self, width: u32, height: u32) -> RenderState {
        let scale = self.world_scale(width as f64, height as f64);
        let radius_px = (scale * self.settings.piece_radius).max(1.0);
        let shape = if self.settings.shape == ShapeKind::Circle
            || self.settings.board == BoardKind::ContinuousArchimedean
        {
            1
        } else {
            0
        };

        RenderState {
            width,
            height,
            scale: scale as f32,
            point_size: (radius_px * 2.0) as f32,
            shape,
        }
    }

    fn export_spec(&self) -> ExportSpec {
        let radius = self.settings.radius.max(1.0);
        let board = self.settings.board;
        let square_pixel_cells =
            board == BoardKind::LatticeSquare && self.settings.shape == ShapeKind::Square;
        let scale = if square_pixel_cells { 1.0 } else { 4.0 };
        let piece_radius = self.settings.piece_radius.max(0.0);
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
        };

        let width = ((max_x - min_x) * scale).round() as u32 + 1;
        let height = ((max_y - min_y) * scale).round() as u32 + 1;
        let shape = if self.settings.shape == ShapeKind::Circle
            || board == BoardKind::ContinuousArchimedean
        {
            ExportShape::Circle
        } else {
            ExportShape::Square
        };

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
        BoardKind::ContinuousArchimedean => "ContinuousArchimedean",
    }
}
