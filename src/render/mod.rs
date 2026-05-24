use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::{Clamped, JsCast};
use web_sys::{
    Blob, CanvasRenderingContext2d, HtmlAnchorElement, HtmlCanvasElement, ImageData, Url,
    WebGlBuffer, WebGlProgram, WebGlRenderingContext as Gl, WebGlShader, WebGlUniformLocation,
};

use crate::math::{ArchimedeanSpiral, AxialCoord, SquareCoord, TriangleCoord, TriangleSpiral};
use crate::protocol::{
    AttackOverlayUpdate, BoardKind, ColorState, DisplayMode, EngineSettings, ShapeKind,
    VertexBufferUpdate,
};

const FLOATS_PER_VERTEX: usize = 5;
const FLOATS_PER_CIRCLE_VERTEX: usize = 8;
const CIRCLE_VERTICES_PER_QUAD: usize = 6;
const BYTES_PER_FLOAT: usize = std::mem::size_of::<f32>();
const PLACEMENT_VERTEX_PAGE_VERTICES: usize = 1_048_576;
const PLACEMENT_VERTEX_PAGE_FLOATS: usize = PLACEMENT_VERTEX_PAGE_VERTICES * FLOATS_PER_VERTEX;
const MAX_TRACK_POINTS: usize = 160_000;
const MAX_BORDER_POINTS: usize = 4_096;
const EXPORT_ENCODER_TIMEOUT_MS: i32 = 30_000;
type ExportTick = Rc<RefCell<Option<Closure<dyn FnMut()>>>>;

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

const CIRCLE_VERTEX_SHADER: &str = r#"
attribute vec2 a_position;
attribute vec2 a_center;
attribute float a_radius;
attribute vec3 a_color;

uniform vec2 u_resolution;
uniform float u_scale;
uniform vec2 u_pan;
uniform highp float u_line_width;

varying highp vec2 v_position;
varying highp vec2 v_center;
varying highp float v_radius;
varying vec3 v_color;

void main() {
    highp vec2 world_position = a_center + a_position * (a_radius + u_line_width);
    vec2 screen = vec2(
        u_resolution.x * 0.5 + (world_position.x + u_pan.x) * u_scale,
        u_resolution.y * 0.5 - (world_position.y + u_pan.y) * u_scale
    );
    vec2 clip = vec2(
        (screen.x / u_resolution.x) * 2.0 - 1.0,
        1.0 - (screen.y / u_resolution.y) * 2.0
    );

    gl_Position = vec4(clip, 0.0, 1.0);
    v_position = world_position;
    v_center = a_center;
    v_radius = a_radius;
    v_color = a_color;
}
"#;

const CIRCLE_FRAGMENT_SHADER: &str = r#"
precision mediump float;

uniform float u_alpha;
uniform highp float u_line_width;

varying highp vec2 v_position;
varying highp vec2 v_center;
varying highp float v_radius;
varying vec3 v_color;

void main() {
    highp float delta = abs(distance(v_position, v_center) - v_radius);
    if (delta > u_line_width) {
        discard;
    }

    float coverage = 1.0 - smoothstep(u_line_width * 0.65, u_line_width, delta);
    gl_FragColor = vec4(v_color, u_alpha * coverage);
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
    circle_program: WebGlProgram,
    position_attrib: u32,
    color_attrib: u32,
    circle_position_attrib: u32,
    circle_center_attrib: u32,
    circle_radius_attrib: u32,
    circle_color_attrib: u32,
    resolution_uniform: WebGlUniformLocation,
    scale_uniform: WebGlUniformLocation,
    point_size_uniform: WebGlUniformLocation,
    pan_uniform: WebGlUniformLocation,
    shape_uniform: WebGlUniformLocation,
    alpha_uniform: WebGlUniformLocation,
    saturation_uniform: WebGlUniformLocation,
    circle_resolution_uniform: WebGlUniformLocation,
    circle_scale_uniform: WebGlUniformLocation,
    circle_pan_uniform: WebGlUniformLocation,
    circle_alpha_uniform: WebGlUniformLocation,
    circle_line_width_uniform: WebGlUniformLocation,
    vertices: Vec<f32>,
    attack_spot_vertices: Vec<f32>,
    attack_hit_vertices: Vec<f32>,
    attack_circle_vertices: Vec<f32>,
    track_vertices: Vec<f32>,
    border_vertices: Vec<f32>,
    pending_upload: PendingUpload,
    attack_spot_upload_pending: bool,
    attack_hit_upload_pending: bool,
    attack_circle_upload_pending: bool,
    track_upload_pending: bool,
    border_upload_pending: bool,
    placement_pages: Vec<VertexPage>,
    attack_spot_buffer: WebGlBuffer,
    attack_hit_buffer: WebGlBuffer,
    attack_circle_buffer: WebGlBuffer,
    track_buffer: WebGlBuffer,
    border_buffer: WebGlBuffer,
    settings: EngineSettings,
    placement_settings: EngineSettings,
    color_state: ColorState,
    color_saturation: f32,
    generation_border_visible: bool,
    pan_x: f64,
    pan_y: f64,
    track_key: Option<TrackKey>,
    border_key: Option<BorderKey>,
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
        let circle_program = link_program(
            &gl,
            &compile_shader(&gl, Gl::VERTEX_SHADER, CIRCLE_VERTEX_SHADER)?,
            &compile_shader(&gl, Gl::FRAGMENT_SHADER, CIRCLE_FRAGMENT_SHADER)?,
        )?;
        gl.use_program(Some(&program));

        let position_attrib = attrib_location(&gl, &program, "a_position")?;
        let color_attrib = attrib_location(&gl, &program, "a_color")?;
        let resolution_uniform = uniform_location(&gl, &program, "u_resolution")?;
        let scale_uniform = uniform_location(&gl, &program, "u_scale")?;
        let point_size_uniform = uniform_location(&gl, &program, "u_point_size")?;
        let pan_uniform = uniform_location(&gl, &program, "u_pan")?;
        let shape_uniform = uniform_location(&gl, &program, "u_shape")?;
        let alpha_uniform = uniform_location(&gl, &program, "u_alpha")?;
        let saturation_uniform = uniform_location(&gl, &program, "u_saturation")?;
        let circle_position_attrib = attrib_location(&gl, &circle_program, "a_position")?;
        let circle_center_attrib = attrib_location(&gl, &circle_program, "a_center")?;
        let circle_radius_attrib = attrib_location(&gl, &circle_program, "a_radius")?;
        let circle_color_attrib = attrib_location(&gl, &circle_program, "a_color")?;
        let circle_resolution_uniform = uniform_location(&gl, &circle_program, "u_resolution")?;
        let circle_scale_uniform = uniform_location(&gl, &circle_program, "u_scale")?;
        let circle_pan_uniform = uniform_location(&gl, &circle_program, "u_pan")?;
        let circle_alpha_uniform = uniform_location(&gl, &circle_program, "u_alpha")?;
        let circle_line_width_uniform = uniform_location(&gl, &circle_program, "u_line_width")?;
        let track_buffer = gl
            .create_buffer()
            .ok_or_else(|| JsValue::from_str("failed to create WebGL track buffer"))?;
        let border_buffer = gl
            .create_buffer()
            .ok_or_else(|| JsValue::from_str("failed to create WebGL radius border buffer"))?;
        let attack_spot_buffer = gl
            .create_buffer()
            .ok_or_else(|| JsValue::from_str("failed to create WebGL attack spot buffer"))?;
        let attack_hit_buffer = gl
            .create_buffer()
            .ok_or_else(|| JsValue::from_str("failed to create WebGL attack hit buffer"))?;
        let attack_circle_buffer = gl
            .create_buffer()
            .ok_or_else(|| JsValue::from_str("failed to create WebGL attack circle buffer"))?;

        gl.clear_color(0.031, 0.035, 0.039, 1.0);
        gl.disable(Gl::DEPTH_TEST);
        gl.enable(Gl::BLEND);
        gl.blend_func(Gl::SRC_ALPHA, Gl::ONE_MINUS_SRC_ALPHA);

        let mut renderer = Self {
            canvas,
            gl,
            program,
            circle_program,
            position_attrib,
            color_attrib,
            circle_position_attrib,
            circle_center_attrib,
            circle_radius_attrib,
            circle_color_attrib,
            resolution_uniform,
            scale_uniform,
            point_size_uniform,
            pan_uniform,
            shape_uniform,
            alpha_uniform,
            saturation_uniform,
            circle_resolution_uniform,
            circle_scale_uniform,
            circle_pan_uniform,
            circle_alpha_uniform,
            circle_line_width_uniform,
            vertices: Vec::new(),
            attack_spot_vertices: Vec::new(),
            attack_hit_vertices: Vec::new(),
            attack_circle_vertices: Vec::new(),
            track_vertices: Vec::new(),
            border_vertices: Vec::new(),
            pending_upload: PendingUpload::Full,
            attack_spot_upload_pending: true,
            attack_hit_upload_pending: true,
            attack_circle_upload_pending: true,
            track_upload_pending: true,
            border_upload_pending: true,
            placement_pages: Vec::new(),
            attack_spot_buffer,
            attack_hit_buffer,
            attack_circle_buffer,
            track_buffer,
            border_buffer,
            settings: EngineSettings::default(),
            placement_settings: EngineSettings::default(),
            color_state: ColorState::default(),
            color_saturation: 1.0,
            generation_border_visible: true,
            pan_x: 0.0,
            pan_y: 0.0,
            track_key: None,
            border_key: None,
        };
        renderer
            .canvas
            .set_attribute("data-generation-border", "visible")?;
        renderer.resize_to_viewport()?;
        Ok(renderer)
    }

    pub fn set_settings(&mut self, settings: EngineSettings) -> Result<(), JsValue> {
        self.set_snapshot_settings(settings.clone(), settings)
    }

    pub fn set_snapshot_settings(
        &mut self,
        view_settings: EngineSettings,
        placement_settings: EngineSettings,
    ) -> Result<(), JsValue> {
        let next_track_key = TrackKey::from_settings(&view_settings);
        if next_track_key != self.track_key {
            self.track_vertices = build_track_vertices(&view_settings);
            self.track_upload_pending = true;
            self.track_key = next_track_key;
        }
        let next_border_key = BorderKey::from_settings(&view_settings);
        if next_border_key != self.border_key {
            self.border_vertices = build_generation_border_vertices(&view_settings);
            self.border_upload_pending = true;
            self.border_key = next_border_key;
        }
        self.settings = view_settings;
        self.placement_settings = placement_settings;
        self.canvas.set_attribute(
            "data-piece-shape",
            shape_data_value(self.placement_settings.shape),
        )?;
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

    pub fn set_generation_border_visible(&mut self, visible: bool) -> Result<(), JsValue> {
        self.generation_border_visible = visible;
        self.canvas.set_attribute(
            "data-generation-border",
            if visible { "visible" } else { "hidden" },
        )?;
        self.redraw()
    }

    pub fn clear_placements(&mut self) -> Result<(), JsValue> {
        self.vertices.clear();
        self.attack_spot_vertices.clear();
        self.attack_hit_vertices.clear();
        self.attack_circle_vertices.clear();
        self.pending_upload = PendingUpload::Full;
        self.attack_spot_upload_pending = true;
        self.attack_hit_upload_pending = true;
        self.attack_circle_upload_pending = true;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
        self.sync_attack_overlay_attributes()?;
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
        let bounds = self.view_bounds(rendered_piece_radius(&self.placement_settings));
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
        attack_overlay_update: &AttackOverlayUpdate,
        color_state: ColorState,
    ) -> Result<(), JsValue> {
        self.color_state = color_state;
        self.apply_vertex_update(vertex_update);
        self.apply_attack_overlay_update(attack_overlay_update);
        Ok(())
    }

    pub fn apply_stats(
        &mut self,
        vertex_update: &VertexBufferUpdate,
        attack_overlay_update: &AttackOverlayUpdate,
        color_state: ColorState,
    ) -> Result<(), JsValue> {
        self.color_state = color_state;
        self.apply_vertex_update(vertex_update);
        self.apply_attack_overlay_update(attack_overlay_update);
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

    fn apply_attack_overlay_update(&mut self, update: &AttackOverlayUpdate) {
        apply_static_vertex_update(
            &mut self.attack_spot_vertices,
            &mut self.attack_spot_upload_pending,
            &update.spots,
        );
        apply_static_vertex_update(
            &mut self.attack_hit_vertices,
            &mut self.attack_hit_upload_pending,
            &update.hits,
        );
        apply_static_vertex_update(
            &mut self.attack_circle_vertices,
            &mut self.attack_circle_upload_pending,
            &update.circles,
        );
        if let Err(error) = self.sync_attack_overlay_attributes() {
            web_sys::console::error_1(&error);
        }
    }

    fn sync_attack_overlay_attributes(&self) -> Result<(), JsValue> {
        self.canvas.set_attribute(
            "data-attack-spots",
            &(self.attack_spot_vertices.len() / FLOATS_PER_VERTEX).to_string(),
        )?;
        self.canvas.set_attribute(
            "data-attack-hits",
            &(self.attack_hit_vertices.len() / FLOATS_PER_VERTEX).to_string(),
        )?;
        self.canvas.set_attribute(
            "data-attack-circles",
            &(self.attack_circle_vertices.len()
                / (FLOATS_PER_CIRCLE_VERTEX * CIRCLE_VERTICES_PER_QUAD))
                .to_string(),
        )?;
        Ok(())
    }

    fn sync_placement_attributes(&self) -> Result<(), JsValue> {
        self.canvas.set_attribute(
            "data-placements",
            &(self.vertices.len() / FLOATS_PER_VERTEX).to_string(),
        )?;
        self.canvas.set_attribute(
            "data-placement-pages",
            &self
                .vertices
                .len()
                .div_ceil(PLACEMENT_VERTEX_PAGE_FLOATS)
                .to_string(),
        )?;
        Ok(())
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
        } else if self.generation_border_visible && !self.border_vertices.is_empty() {
            self.draw_generation_border()?;
        }

        if self.settings.attack_overlay_opacity > f32::EPSILON
            && !self.attack_spot_vertices.is_empty()
        {
            self.draw_attack_spots(&render_state)?;
        }

        if self.vertices.is_empty() {
            self.pending_upload = PendingUpload::None;
            self.sync_placement_attributes()?;
            return Ok(());
        }

        self.sync_gpu_buffer()?;
        self.sync_placement_attributes()?;
        self.gl
            .uniform1f(Some(&self.point_size_uniform), render_state.point_size);
        self.gl
            .uniform1i(Some(&self.shape_uniform), render_state.shape);
        self.gl.uniform1f(Some(&self.alpha_uniform), 1.0);
        self.gl.uniform1f(
            Some(&self.saturation_uniform),
            self.color_saturation.clamp(0.0, 1.0),
        );
        self.draw_placement_pages()?;

        if self.settings.attack_overlay_opacity > f32::EPSILON {
            if !self.attack_hit_vertices.is_empty() {
                self.draw_attack_hits(&render_state)?;
            }
            if !self.attack_circle_vertices.is_empty() {
                self.draw_attack_circles()?;
            }
        }

        Ok(())
    }

    fn draw_attack_spots(&mut self, render_state: &RenderState) -> Result<(), JsValue> {
        upload_static_vertices(
            &self.gl,
            &self.attack_spot_buffer,
            &self.attack_spot_vertices,
            &mut self.attack_spot_upload_pending,
        );
        self.configure_vertex_attribs();
        self.gl
            .uniform1f(Some(&self.point_size_uniform), render_state.point_size);
        self.gl
            .uniform1i(Some(&self.shape_uniform), render_state.shape);
        self.gl.uniform1f(
            Some(&self.alpha_uniform),
            self.settings.attack_overlay_opacity.clamp(0.0, 1.0),
        );
        self.gl.uniform1f(Some(&self.saturation_uniform), 1.0);
        self.gl.draw_arrays(
            Gl::POINTS,
            0,
            (self.attack_spot_vertices.len() / FLOATS_PER_VERTEX) as i32,
        );
        Ok(())
    }

    fn draw_attack_hits(&mut self, render_state: &RenderState) -> Result<(), JsValue> {
        upload_static_vertices(
            &self.gl,
            &self.attack_hit_buffer,
            &self.attack_hit_vertices,
            &mut self.attack_hit_upload_pending,
        );
        self.configure_vertex_attribs();
        self.gl
            .uniform1f(Some(&self.point_size_uniform), render_state.point_size);
        self.gl
            .uniform1i(Some(&self.shape_uniform), render_state.shape);
        self.gl.uniform1f(
            Some(&self.alpha_uniform),
            (self.settings.attack_overlay_opacity * 0.5).clamp(0.0, 0.5),
        );
        self.gl.uniform1f(Some(&self.saturation_uniform), 1.0);
        self.gl.draw_arrays(
            Gl::POINTS,
            0,
            (self.attack_hit_vertices.len() / FLOATS_PER_VERTEX) as i32,
        );
        Ok(())
    }

    fn draw_attack_circles(&mut self) -> Result<(), JsValue> {
        upload_static_vertices(
            &self.gl,
            &self.attack_circle_buffer,
            &self.attack_circle_vertices,
            &mut self.attack_circle_upload_pending,
        );
        let width = self.canvas.width();
        let height = self.canvas.height();
        let render_state = self.render_state(width, height);

        self.gl.use_program(Some(&self.circle_program));
        self.gl.uniform2f(
            Some(&self.circle_resolution_uniform),
            width as f32,
            height as f32,
        );
        self.gl
            .uniform1f(Some(&self.circle_scale_uniform), render_state.scale);
        self.gl.uniform2f(
            Some(&self.circle_pan_uniform),
            (self.pan_x - render_state.center_x) as f32,
            (self.pan_y - render_state.center_y) as f32,
        );
        self.gl.uniform1f(
            Some(&self.circle_alpha_uniform),
            self.settings.attack_overlay_opacity.clamp(0.0, 1.0),
        );
        self.gl.uniform1f(
            Some(&self.circle_line_width_uniform),
            1.25_f32 / render_state.scale.max(f32::EPSILON),
        );
        self.configure_circle_vertex_attribs();
        self.gl.draw_arrays(
            Gl::TRIANGLES,
            0,
            (self.attack_circle_vertices.len() / FLOATS_PER_CIRCLE_VERTEX) as i32,
        );
        self.gl.use_program(Some(&self.program));
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

    fn draw_generation_border(&mut self) -> Result<(), JsValue> {
        self.gl
            .bind_buffer(Gl::ARRAY_BUFFER, Some(&self.border_buffer));
        if self.border_upload_pending {
            unsafe {
                let view = js_sys::Float32Array::view(&self.border_vertices);
                self.gl.buffer_data_with_array_buffer_view(
                    Gl::ARRAY_BUFFER,
                    &view,
                    Gl::STATIC_DRAW,
                );
            }
            self.border_upload_pending = false;
        }

        self.configure_vertex_attribs();
        self.gl.uniform1f(Some(&self.point_size_uniform), 1.0);
        self.gl.uniform1i(Some(&self.shape_uniform), 0);
        self.gl.uniform1f(Some(&self.alpha_uniform), 0.55);
        self.gl.uniform1f(Some(&self.saturation_uniform), 1.0);
        self.gl.draw_arrays(
            Gl::LINE_STRIP,
            0,
            (self.border_vertices.len() / FLOATS_PER_VERTEX) as i32,
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

    fn configure_circle_vertex_attribs(&self) {
        let stride = (FLOATS_PER_CIRCLE_VERTEX * std::mem::size_of::<f32>()) as i32;
        self.gl
            .enable_vertex_attrib_array(self.circle_position_attrib);
        self.gl.vertex_attrib_pointer_with_i32(
            self.circle_position_attrib,
            2,
            Gl::FLOAT,
            false,
            stride,
            0,
        );
        self.gl
            .enable_vertex_attrib_array(self.circle_center_attrib);
        self.gl.vertex_attrib_pointer_with_i32(
            self.circle_center_attrib,
            2,
            Gl::FLOAT,
            false,
            stride,
            (2 * BYTES_PER_FLOAT) as i32,
        );
        self.gl
            .enable_vertex_attrib_array(self.circle_radius_attrib);
        self.gl.vertex_attrib_pointer_with_i32(
            self.circle_radius_attrib,
            1,
            Gl::FLOAT,
            false,
            stride,
            (4 * BYTES_PER_FLOAT) as i32,
        );
        self.gl.enable_vertex_attrib_array(self.circle_color_attrib);
        self.gl.vertex_attrib_pointer_with_i32(
            self.circle_color_attrib,
            3,
            Gl::FLOAT,
            false,
            stride,
            (5 * BYTES_PER_FLOAT) as i32,
        );
    }

    pub fn download_image(
        &self,
        mime_type: &str,
        filename: &str,
        kind: ExportKind,
        encoder_quality: Option<f64>,
        cancel_flag: Rc<Cell<bool>>,
        finish: Rc<dyn Fn(Result<(), String>)>,
    ) -> Result<(), JsValue> {
        let job = Rc::new(RefCell::new(ExportJob::new(self, kind)?));
        let mime_type = mime_type.to_string();
        let filename = filename.to_string();
        let quality = encoder_quality.unwrap_or(0.92);
        run_export_job(job, mime_type, filename, quality, cancel_flag, finish)?;
        Ok(())
    }

    fn sync_gpu_buffer(&mut self) -> Result<(), JsValue> {
        match self.pending_upload {
            PendingUpload::None => Ok(()),
            PendingUpload::Full => {
                self.reset_placement_page_uploads();
                self.upload_vertex_range(0, self.vertices.len())?;
                self.pending_upload = PendingUpload::None;
                Ok(())
            }
            PendingUpload::Append { start_float } => {
                let total = self.vertices.len();
                let start_float = start_float.min(total);
                self.upload_vertex_range(start_float, total)?;
                self.pending_upload = PendingUpload::None;
                Ok(())
            }
        }
    }

    fn ensure_placement_pages(&mut self, required_floats: usize) -> Result<(), JsValue> {
        let required_pages = required_floats.div_ceil(PLACEMENT_VERTEX_PAGE_FLOATS);
        while self.placement_pages.len() < required_pages {
            self.placement_pages.push(VertexPage {
                buffer: self
                    .gl
                    .create_buffer()
                    .ok_or_else(|| JsValue::from_str("failed to create WebGL placement buffer"))?,
                allocated: false,
                uploaded_floats: 0,
            });
        }
        Ok(())
    }

    fn reset_placement_page_uploads(&mut self) {
        for page in &mut self.placement_pages {
            page.uploaded_floats = 0;
        }
    }

    fn upload_vertex_range(&mut self, start_float: usize, end_float: usize) -> Result<(), JsValue> {
        if start_float >= end_float {
            return Ok(());
        }

        self.ensure_placement_pages(end_float)?;
        let page_bytes = PLACEMENT_VERTEX_PAGE_FLOATS
            .checked_mul(BYTES_PER_FLOAT)
            .and_then(|bytes| i32::try_from(bytes).ok())
            .ok_or_else(|| JsValue::from_str("WebGL placement page is too large"))?;
        let first_page = start_float / PLACEMENT_VERTEX_PAGE_FLOATS;
        let last_page = (end_float - 1) / PLACEMENT_VERTEX_PAGE_FLOATS;

        for page_index in first_page..=last_page {
            let page_start = page_index * PLACEMENT_VERTEX_PAGE_FLOATS;
            let local_start = start_float.saturating_sub(page_start);
            let local_end = (end_float - page_start).min(PLACEMENT_VERTEX_PAGE_FLOATS);
            if local_start >= local_end {
                continue;
            }

            let page = &mut self.placement_pages[page_index];
            self.gl.bind_buffer(Gl::ARRAY_BUFFER, Some(&page.buffer));
            if !page.allocated {
                self.gl
                    .buffer_data_with_i32(Gl::ARRAY_BUFFER, page_bytes, Gl::DYNAMIC_DRAW);
                page.allocated = true;
            }

            let offset_bytes = local_start
                .checked_mul(BYTES_PER_FLOAT)
                .and_then(|bytes| i32::try_from(bytes).ok())
                .ok_or_else(|| JsValue::from_str("WebGL vertex upload offset is too large"))?;
            let global_start = page_start + local_start;
            let global_end = page_start + local_end;

            unsafe {
                let view = js_sys::Float32Array::view(&self.vertices[global_start..global_end]);
                self.gl.buffer_sub_data_with_i32_and_array_buffer_view(
                    Gl::ARRAY_BUFFER,
                    offset_bytes,
                    &view,
                );
            }
            page.uploaded_floats = page.uploaded_floats.max(local_end);
        }

        Ok(())
    }

    fn draw_placement_pages(&self) -> Result<(), JsValue> {
        let total = self.vertices.len();
        let page_count = total.div_ceil(PLACEMENT_VERTEX_PAGE_FLOATS);
        for page_index in 0..page_count {
            let Some(page) = self.placement_pages.get(page_index) else {
                continue;
            };
            let start = page_index * PLACEMENT_VERTEX_PAGE_FLOATS;
            let end = (start + PLACEMENT_VERTEX_PAGE_FLOATS).min(total);
            let draw_floats = end.saturating_sub(start).min(page.uploaded_floats);
            if draw_floats < FLOATS_PER_VERTEX {
                continue;
            }

            self.gl.bind_buffer(Gl::ARRAY_BUFFER, Some(&page.buffer));
            self.configure_vertex_attribs();
            self.gl
                .draw_arrays(Gl::POINTS, 0, (draw_floats / FLOATS_PER_VERTEX) as i32);
        }
        Ok(())
    }

    fn world_scale(&self, width: f64, height: f64) -> f64 {
        world_scale_for_settings_with_margin(
            &self.settings,
            rendered_piece_radius(&self.placement_settings),
            width,
            height,
        )
    }

    fn view_bounds(&self, margin: f64) -> WorldBounds {
        board_world_bounds(&self.settings, margin)
    }

    fn render_state(&self, width: u32, height: u32) -> RenderState {
        let scale = self.world_scale(width as f64, height as f64);
        let radius_px = (scale * rendered_piece_radius(&self.placement_settings)).max(1.0);
        let shape = shader_shape(&self.placement_settings);
        let bounds = self.view_bounds(rendered_piece_radius(&self.placement_settings));

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
            board == BoardKind::LatticeSquare && self.placement_settings.shape == ShapeKind::Square;
        let piece_radius = rendered_piece_radius(&self.placement_settings).max(0.0);
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
        let shape = export_shape(&self.placement_settings);

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

        let bounds = self.view_bounds(rendered_piece_radius(&self.placement_settings));
        let half_board_width = bounds.width() * 0.5;
        let half_board_height = bounds.height() * 0.5;
        let half_width = self.canvas.width() as f64 / (2.0 * scale);
        let half_height = self.canvas.height() as f64 / (2.0 * scale);
        let edge_room = half_width.min(half_height) * 0.25
            + rendered_piece_radius(&self.placement_settings)
            + 4.0;
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

struct VertexPage {
    buffer: WebGlBuffer,
    allocated: bool,
    uploaded_floats: usize,
}

fn apply_static_vertex_update(
    target: &mut Vec<f32>,
    upload_pending: &mut bool,
    update: &VertexBufferUpdate,
) {
    match update {
        VertexBufferUpdate::None => {}
        VertexBufferUpdate::Append(vertices) => {
            target.extend_from_slice(vertices);
            *upload_pending = true;
        }
        VertexBufferUpdate::Replace(vertices) => {
            target.clear();
            target.extend_from_slice(vertices);
            *upload_pending = true;
        }
    }
}

fn upload_static_vertices(
    gl: &Gl,
    buffer: &WebGlBuffer,
    vertices: &[f32],
    upload_pending: &mut bool,
) {
    gl.bind_buffer(Gl::ARRAY_BUFFER, Some(buffer));
    if *upload_pending {
        unsafe {
            let view = js_sys::Float32Array::view(vertices);
            gl.buffer_data_with_array_buffer_view(Gl::ARRAY_BUFFER, &view, Gl::STATIC_DRAW);
        }
        *upload_pending = false;
    }
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

enum ExportJob {
    Direct {
        spec: ExportSpec,
        vertices: Vec<f32>,
        pixels: Vec<u8>,
        stage: ExportStage,
    },
    Supersampled {
        spec: ExportSpec,
        supersampled: ExportSpec,
        vertices: Vec<f32>,
        high_pixels: Vec<u8>,
        pixels: Vec<u8>,
        stage: ExportStage,
    },
}

enum ExportStage {
    Fill { next_pixel: usize },
    DrawPieces { next_vertex: usize },
    Downsample { next_row: u32 },
    Finish,
    Done,
}

enum ExportStep {
    Pending,
    Complete(HtmlCanvasElement),
}

impl ExportJob {
    fn new(renderer: &CanvasRenderer, kind: ExportKind) -> Result<Self, JsValue> {
        let spec = renderer.export_spec(kind, 1.0);
        let pixel_count = checked_pixel_count(spec.width, spec.height)?;
        let pixels = allocate_pixel_buffer(pixel_count)?;
        let vertices = renderer.vertices.clone();

        if spec.square_pixel_cells {
            return Ok(Self::Direct {
                spec,
                vertices,
                pixels,
                stage: ExportStage::Fill { next_pixel: 0 },
            });
        }

        let supersampled = renderer.export_spec(kind, 2.0);
        let supersampled_pixel_count =
            checked_pixel_count(supersampled.width, supersampled.height)?;
        let high_pixels = allocate_pixel_buffer(supersampled_pixel_count)?;

        Ok(Self::Supersampled {
            spec,
            supersampled,
            vertices,
            high_pixels,
            pixels,
            stage: ExportStage::Fill { next_pixel: 0 },
        })
    }

    fn step(&mut self) -> Result<ExportStep, JsValue> {
        const PIXELS_PER_CHUNK: usize = 250_000;
        const DIRECT_VERTICES_PER_CHUNK: usize = 8_192;
        const SQUARE_PIXEL_VERTICES_PER_CHUNK: usize = 100_000;
        const SUPERSAMPLED_VERTICES_PER_CHUNK: usize = 1_024;
        const DOWNSAMPLE_ROWS_PER_CHUNK: u32 = 64;

        match self {
            Self::Direct {
                spec,
                vertices,
                pixels,
                stage,
            } => match stage {
                ExportStage::Fill { next_pixel } => {
                    let total_pixels = pixels.len() / 4;
                    let end = next_pixel
                        .saturating_add(PIXELS_PER_CHUNK)
                        .min(total_pixels);
                    fill_background_range(pixels, *next_pixel, end);
                    if end >= total_pixels {
                        *stage = ExportStage::DrawPieces { next_vertex: 0 };
                    } else {
                        *next_pixel = end;
                    }
                    Ok(ExportStep::Pending)
                }
                ExportStage::DrawPieces { next_vertex } => {
                    let total_vertices = vertices.len() / FLOATS_PER_VERTEX;
                    let vertices_per_chunk = if spec.square_pixel_cells {
                        SQUARE_PIXEL_VERTICES_PER_CHUNK
                    } else {
                        DIRECT_VERTICES_PER_CHUNK
                    };
                    let end = next_vertex
                        .saturating_add(vertices_per_chunk)
                        .min(total_vertices);
                    draw_export_vertices(pixels, spec, vertices, *next_vertex, end);
                    if end >= total_vertices {
                        *stage = ExportStage::Finish;
                    } else {
                        *next_vertex = end;
                    }
                    Ok(ExportStep::Pending)
                }
                ExportStage::Finish => {
                    *stage = ExportStage::Done;
                    canvas_from_pixels(spec.width, spec.height, std::mem::take(pixels))
                        .map(ExportStep::Complete)
                }
                ExportStage::Done => Ok(ExportStep::Pending),
                ExportStage::Downsample { .. } => Ok(ExportStep::Pending),
            },
            Self::Supersampled {
                spec,
                supersampled,
                vertices,
                high_pixels,
                pixels,
                stage,
            } => match stage {
                ExportStage::Fill { next_pixel } => {
                    let total_pixels = high_pixels.len() / 4;
                    let end = next_pixel
                        .saturating_add(PIXELS_PER_CHUNK)
                        .min(total_pixels);
                    fill_background_range(high_pixels, *next_pixel, end);
                    if end >= total_pixels {
                        *stage = ExportStage::DrawPieces { next_vertex: 0 };
                    } else {
                        *next_pixel = end;
                    }
                    Ok(ExportStep::Pending)
                }
                ExportStage::DrawPieces { next_vertex } => {
                    let total_vertices = vertices.len() / FLOATS_PER_VERTEX;
                    let end = next_vertex
                        .saturating_add(SUPERSAMPLED_VERTICES_PER_CHUNK)
                        .min(total_vertices);
                    draw_export_vertices(high_pixels, supersampled, vertices, *next_vertex, end);
                    if end >= total_vertices {
                        *stage = ExportStage::Downsample { next_row: 0 };
                    } else {
                        *next_vertex = end;
                    }
                    Ok(ExportStep::Pending)
                }
                ExportStage::Downsample { next_row } => {
                    let end = next_row
                        .saturating_add(DOWNSAMPLE_ROWS_PER_CHUNK)
                        .min(spec.height);
                    downsample_2x_rows(
                        high_pixels,
                        supersampled.width,
                        supersampled.height,
                        pixels,
                        spec.width,
                        *next_row,
                        end,
                    );
                    if end >= spec.height {
                        *stage = ExportStage::Finish;
                    } else {
                        *next_row = end;
                    }
                    Ok(ExportStep::Pending)
                }
                ExportStage::Finish => {
                    *stage = ExportStage::Done;
                    canvas_from_pixels(spec.width, spec.height, std::mem::take(pixels))
                        .map(ExportStep::Complete)
                }
                ExportStage::Done => Ok(ExportStep::Pending),
            },
        }
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BorderKey {
    board: BoardKind,
    radius_key: u64,
    track_hidden: bool,
}

impl BorderKey {
    fn from_settings(settings: &EngineSettings) -> Option<Self> {
        let track_hidden = settings.track_opacity <= f32::EPSILON;
        if !track_hidden {
            return None;
        }

        Some(Self {
            board: settings.board,
            radius_key: match settings.board {
                BoardKind::ContinuousArchimedean => settings.radius.max(0.0).to_bits(),
                _ => settings.radius.max(0.0).floor().to_bits(),
            },
            track_hidden,
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

fn shape_data_value(shape: ShapeKind) -> &'static str {
    match shape {
        ShapeKind::Square => "Square",
        ShapeKind::Circle => "Circle",
        ShapeKind::Hex => "Hex",
        ShapeKind::Triangle => "Triangle",
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

#[cfg(test)]
fn world_scale_for_settings(settings: &EngineSettings, width: f64, height: f64) -> f64 {
    world_scale_for_settings_with_margin(settings, rendered_piece_radius(settings), width, height)
}

fn world_scale_for_settings_with_margin(
    settings: &EngineSettings,
    margin: f64,
    width: f64,
    height: f64,
) -> f64 {
    let fit_scale = fit_screen_scale(width, height, board_world_bounds(settings, margin));
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
            push_square_track_vertices(&mut vertices, bound);
        }
        BoardKind::LatticeHex => {
            let bound = settings.radius.max(0.0).floor() as u64;
            push_hex_track_vertices(&mut vertices, bound);
        }
        BoardKind::LatticeTriangle => {
            let bound = settings.radius.max(0.0).floor() as u64;
            push_triangle_track_vertices(&mut vertices, bound);
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

fn build_generation_border_vertices(settings: &EngineSettings) -> Vec<f32> {
    if settings.track_opacity > f32::EPSILON {
        return Vec::new();
    }

    let mut vertices = Vec::new();
    match settings.board {
        BoardKind::LatticeSquare => {
            let bound = settings.radius.max(0.0).floor();
            for (x, y) in [
                (-bound, -bound),
                (bound, -bound),
                (bound, bound),
                (-bound, bound),
                (-bound, -bound),
            ] {
                push_border_vertex(&mut vertices, x, y);
            }
        }
        BoardKind::LatticeHex => {
            let bound = settings.radius.max(0.0).floor() as i64;
            let corners = [
                AxialCoord::new(bound, 0),
                AxialCoord::new(0, bound),
                AxialCoord::new(-bound, bound),
                AxialCoord::new(-bound, 0),
                AxialCoord::new(0, -bound),
                AxialCoord::new(bound, -bound),
                AxialCoord::new(bound, 0),
            ];
            for coord in corners {
                let point = coord.to_point();
                push_border_vertex(&mut vertices, point.x, point.y);
            }
        }
        BoardKind::LatticeTriangle => {
            let bound = settings.radius.max(0.0).floor() as u64;
            push_sampled_closed_ring(
                &mut vertices,
                TriangleSpiral::ring(bound).into_iter().map(|coord| {
                    let point = coord.to_point();
                    (point.x, point.y)
                }),
            );
        }
        BoardKind::ContinuousArchimedean => {
            let radius = settings.radius.max(0.0);
            let circumference = std::f64::consts::TAU * radius.max(1.0);
            let segments = (circumference.ceil() as usize).clamp(64, MAX_BORDER_POINTS);
            for step in 0..=segments {
                let theta = std::f64::consts::TAU * step as f64 / segments as f64;
                push_border_vertex(&mut vertices, radius * theta.cos(), radius * theta.sin());
            }
        }
    }

    vertices
}

fn push_sampled_closed_ring<I>(vertices: &mut Vec<f32>, points: I)
where
    I: IntoIterator<Item = (f64, f64)>,
{
    let points = points.into_iter().collect::<Vec<_>>();
    if points.is_empty() {
        return;
    }

    let stride = points.len().div_ceil(MAX_BORDER_POINTS).max(1);
    for point in points.iter().step_by(stride) {
        push_border_vertex(vertices, point.0, point.1);
    }
    push_border_vertex(vertices, points[0].0, points[0].1);
}

fn push_square_track_vertices(vertices: &mut Vec<f32>, bound: u64) {
    reserve_track_vertex_floats(vertices, bound.saturating_mul(5).saturating_add(1));
    push_square_track_coord(vertices, 0, 0);
    for radius in 1..=bound {
        let r = radius as i64;
        for (x, y) in [(r, 1 - r), (r, r), (-r, r), (-r, -r), (r, -r)] {
            push_square_track_coord(vertices, x, y);
        }
    }
}

fn push_square_track_coord(vertices: &mut Vec<f32>, x: i64, y: i64) {
    let point = SquareCoord::new(x, y).to_point();
    push_track_vertex(vertices, point.x, point.y);
}

fn push_hex_track_vertices(vertices: &mut Vec<f32>, bound: u64) {
    const AXIAL_DIRECTIONS: [AxialCoord; 6] = [
        AxialCoord { q: 1, r: 0 },
        AxialCoord { q: 0, r: 1 },
        AxialCoord { q: -1, r: 1 },
        AxialCoord { q: -1, r: 0 },
        AxialCoord { q: 0, r: -1 },
        AxialCoord { q: 1, r: -1 },
    ];

    reserve_track_vertex_floats(vertices, bound.saturating_mul(7).saturating_add(1));
    push_hex_track_coord(vertices, AxialCoord::new(0, 0));
    for radius in 1..=bound {
        let r = radius as i64;
        let mut coord = AxialCoord::new(r, 1 - r);
        push_hex_track_coord(vertices, coord);

        if radius > 1 {
            coord = coord.add(AXIAL_DIRECTIONS[1].scale(radius as i64 - 1));
            push_hex_track_coord(vertices, coord);
        }

        for direction in AXIAL_DIRECTIONS
            .iter()
            .skip(2)
            .chain(AXIAL_DIRECTIONS.iter().take(1))
        {
            coord = coord.add(direction.scale(radius as i64));
            push_hex_track_coord(vertices, coord);
        }
    }
}

fn push_hex_track_coord(vertices: &mut Vec<f32>, coord: AxialCoord) {
    let point = coord.to_point();
    push_track_vertex(vertices, point.x, point.y);
}

fn push_triangle_track_vertices(vertices: &mut Vec<f32>, bound: u64) {
    const TRIANGLE_DIRECTIONS: [TriangleCoord; 3] = [
        TriangleCoord { u: 1, v: 0 },
        TriangleCoord { u: -1, v: 1 },
        TriangleCoord { u: 0, v: -1 },
    ];

    reserve_track_vertex_floats(vertices, bound.saturating_mul(3).saturating_add(1));
    let mut coord = TriangleCoord::new(0, 0);
    push_triangle_track_coord(vertices, coord);
    for segment in 1..=bound.saturating_mul(3) {
        let direction = TRIANGLE_DIRECTIONS[(segment as usize - 1) % TRIANGLE_DIRECTIONS.len()];
        coord = coord.add(direction.scale(segment as i64));
        push_triangle_track_coord(vertices, coord);
    }
}

fn reserve_track_vertex_floats(vertices: &mut Vec<f32>, point_count: u64) {
    if let Some(float_count) = point_count
        .checked_mul(FLOATS_PER_VERTEX as u64)
        .and_then(|count| usize::try_from(count).ok())
    {
        vertices.reserve(float_count);
    }
}

fn push_triangle_track_coord(vertices: &mut Vec<f32>, coord: TriangleCoord) {
    let point = coord.to_point();
    push_track_vertex(vertices, point.x, point.y);
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

fn push_border_vertex(vertices: &mut Vec<f32>, x: f64, y: f64) {
    vertices.extend_from_slice(&[x as f32, y as f32, 0.92, 0.96, 1.0]);
}

fn run_export_job(
    job: Rc<RefCell<ExportJob>>,
    mime_type: String,
    filename: String,
    quality: f64,
    cancel_flag: Rc<Cell<bool>>,
    finish: Rc<dyn Fn(Result<(), String>)>,
) -> Result<(), JsValue> {
    let tick: ExportTick = Rc::new(RefCell::new(None));
    let tick_ref = Rc::clone(&tick);
    let closure = Closure::<dyn FnMut()>::new(move || {
        if cancel_flag.get() {
            finish(Err("Export canceled".to_string()));
            tick_ref.borrow_mut().take();
            return;
        }

        let step = job.borrow_mut().step();
        match step {
            Ok(ExportStep::Pending) => {
                if let Err(error) = schedule_export_tick(&tick_ref) {
                    finish(Err(js_value_text(&error)));
                    tick_ref.borrow_mut().take();
                }
            }
            Ok(ExportStep::Complete(canvas)) => {
                tick_ref.borrow_mut().take();
                if let Err(error) = download_canvas_blob(
                    canvas,
                    &mime_type,
                    &filename,
                    quality,
                    Rc::clone(&cancel_flag),
                    Rc::clone(&finish),
                ) {
                    finish(Err(js_value_text(&error)));
                }
            }
            Err(error) => {
                finish(Err(js_value_text(&error)));
                tick_ref.borrow_mut().take();
            }
        }
    });
    *tick.borrow_mut() = Some(closure);
    schedule_export_tick(&tick)
}

fn schedule_export_tick(tick: &ExportTick) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("window unavailable"))?;
    let Some(callback) = tick
        .borrow()
        .as_ref()
        .map(|closure| closure.as_ref().unchecked_ref::<js_sys::Function>().clone())
    else {
        return Ok(());
    };
    window.set_timeout_with_callback_and_timeout_and_arguments_0(&callback, 0)?;
    Ok(())
}

fn download_canvas_blob(
    canvas: HtmlCanvasElement,
    mime_type: &str,
    filename: &str,
    quality: f64,
    cancel_flag: Rc<Cell<bool>>,
    finish: Rc<dyn Fn(Result<(), String>)>,
) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("window unavailable"))?;
    let document = window
        .document()
        .ok_or_else(|| JsValue::from_str("document unavailable"))?;
    let anchor = document
        .create_element("a")?
        .dyn_into::<HtmlAnchorElement>()?;
    anchor.set_download(filename);

    let completed = Rc::new(Cell::new(false));
    let timeout_completed = Rc::clone(&completed);
    let timeout_finish = Rc::clone(&finish);
    let timeout = Closure::<dyn FnMut()>::new(move || {
        if !timeout_completed.replace(true) {
            timeout_finish(Err("image encoder timed out".to_string()));
        }
    });
    window.set_timeout_with_callback_and_timeout_and_arguments_0(
        timeout.as_ref().unchecked_ref(),
        EXPORT_ENCODER_TIMEOUT_MS,
    )?;
    timeout.forget();

    let callback_completed = Rc::clone(&completed);
    let callback = Closure::<dyn FnMut(JsValue)>::new(move |blob_value: JsValue| {
        if callback_completed.replace(true) {
            return;
        }
        if cancel_flag.get() {
            finish(Err("Export canceled".to_string()));
            return;
        }
        if blob_value.is_null() || blob_value.is_undefined() {
            finish(Err("image encoder returned no blob".to_string()));
            return;
        }

        let blob = blob_value.unchecked_into::<Blob>();
        match Url::create_object_url_with_blob(&blob) {
            Ok(url) => {
                anchor.set_href(&url);
                anchor.click();
                if let Err(error) = Url::revoke_object_url(&url) {
                    finish(Err(js_value_text(&error)));
                    return;
                }
                finish(Ok(()));
            }
            Err(error) => finish(Err(js_value_text(&error))),
        }
    });
    let encode_result = canvas.to_blob_with_type_and_encoder_options(
        callback.as_ref().unchecked_ref(),
        mime_type,
        &JsValue::from_f64(quality),
    );
    if let Err(error) = encode_result {
        completed.set(true);
        return Err(error);
    }
    callback.forget();
    Ok(())
}

fn allocate_pixel_buffer(pixel_count: usize) -> Result<Vec<u8>, JsValue> {
    let len = checked_rgba_len(pixel_count)?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(len)
        .map_err(|_| JsValue::from_str("export pixel buffer allocation failed"))?;
    pixels.resize(len, 0);
    Ok(pixels)
}

fn fill_background_range(pixels: &mut [u8], start_pixel: usize, end_pixel: usize) {
    let start = start_pixel.saturating_mul(4).min(pixels.len());
    let end = end_pixel.saturating_mul(4).min(pixels.len());
    for pixel in pixels[start..end].chunks_exact_mut(4) {
        pixel.copy_from_slice(&[8, 9, 10, 255]);
    }
}

fn draw_export_vertices(
    pixels: &mut [u8],
    spec: &ExportSpec,
    vertices: &[f32],
    start_vertex: usize,
    end_vertex: usize,
) {
    let start = start_vertex
        .saturating_mul(FLOATS_PER_VERTEX)
        .min(vertices.len());
    let end = end_vertex
        .saturating_mul(FLOATS_PER_VERTEX)
        .min(vertices.len());
    for vertex in vertices[start..end].chunks_exact(FLOATS_PER_VERTEX) {
        let center_x = vertex[0] as f64;
        let center_y = vertex[1] as f64;
        let color = [
            channel_to_u8(vertex[2]),
            channel_to_u8(vertex[3]),
            channel_to_u8(vertex[4]),
            255,
        ];
        if spec.square_pixel_cells {
            draw_square_cell_export_pixel(pixels, spec, center_x, center_y, color);
        } else {
            draw_export_piece(pixels, spec, center_x, center_y, color);
        }
    }
}

fn draw_square_cell_export_pixel(
    pixels: &mut [u8],
    spec: &ExportSpec,
    center_x: f64,
    center_y: f64,
    color: [u8; 4],
) {
    let px = ((center_x - spec.min_x) * spec.scale).round() as i32;
    let py = ((spec.max_y - center_y) * spec.scale).round() as i32;
    if px < 0 || py < 0 || px >= spec.width as i32 || py >= spec.height as i32 {
        return;
    }
    set_pixel(pixels, spec.width, px as u32, py as u32, color);
}

fn downsample_2x_rows(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    target: &mut [u8],
    target_width: u32,
    start_y: u32,
    end_y: u32,
) {
    for y in start_y..end_y {
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

fn js_value_text(value: &JsValue) -> String {
    value.as_string().unwrap_or_else(|| format!("{value:?}"))
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
        assert_eq!(export_scale(ExportKind::JpegHalf, false, 0.5), 2.0);
    }

    #[test]
    fn export_dimension_still_allows_reported_8003_case_without_policy_cap() {
        let width = export_dimension(8002.0, 1.0);
        let height = export_dimension(8002.0, 1.0);
        let reported_count = checked_pixel_count(width, height).unwrap();

        assert_eq!((width, height), (8003, 8003));
        assert_eq!(reported_count, 64_048_009);
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
    fn lattice_track_radius_150_draws_exact_turn_vertices() {
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
                BoardKind::LatticeSquare => 1 + 5 * 150_u64,
                BoardKind::LatticeHex => 7 * 150_u64,
                BoardKind::LatticeTriangle => 1 + 3 * 150_u64,
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
    fn lattice_track_radius_300_keeps_full_uncompressed_shape() {
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
                point_count <= 2_100,
                "board={board:?}, points={point_count}"
            );

            let xs = vertices
                .chunks_exact(FLOATS_PER_VERTEX)
                .map(|point| point[0].abs() as f64)
                .fold(0.0, f64::max);
            let ys = vertices
                .chunks_exact(FLOATS_PER_VERTEX)
                .map(|point| point[1].abs() as f64)
                .fold(0.0, f64::max);

            match board {
                BoardKind::LatticeSquare => {
                    assert_eq!(xs, 300.0);
                    assert_eq!(ys, 300.0);
                }
                BoardKind::LatticeHex => {
                    assert!(xs > 500.0, "hex x span compressed to {xs}");
                    assert_eq!(ys, 450.0);
                }
                BoardKind::LatticeTriangle => {
                    assert!(xs > 445.0, "triangle x span compressed to {xs}");
                    assert!(ys > 515.0, "triangle y span compressed to {ys}");
                }
                BoardKind::ContinuousArchimedean => unreachable!(),
            }
        }
    }

    #[test]
    fn lattice_track_extreme_radius_uses_exact_turns_and_reaches_edge() {
        for board in [
            BoardKind::LatticeSquare,
            BoardKind::LatticeHex,
            BoardKind::LatticeTriangle,
        ] {
            let settings = EngineSettings {
                board,
                radius: 1500.0,
                track_opacity: 0.5,
                ..EngineSettings::default()
            };
            let vertices = build_track_vertices(&settings);
            let point_count = vertices.len() / FLOATS_PER_VERTEX;
            assert!(
                point_count <= 10_500,
                "board={board:?}, points={point_count}"
            );
            let last = &vertices[vertices.len() - FLOATS_PER_VERTEX..];
            let distance = match board {
                BoardKind::LatticeSquare => last[0].abs().max(last[1].abs()) as f64,
                BoardKind::LatticeHex | BoardKind::LatticeTriangle => {
                    (last[0] as f64).hypot(last[1] as f64)
                }
                BoardKind::ContinuousArchimedean => unreachable!(),
            };
            assert!(distance > 1400.0, "board={board:?}, distance={distance}");
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
    fn generation_radius_border_draws_only_when_track_hidden() {
        let hidden = EngineSettings {
            board: BoardKind::LatticeSquare,
            radius: 12.0,
            track_opacity: 0.0,
            ..EngineSettings::default()
        };
        let mut visible_track = hidden.clone();
        visible_track.track_opacity = 0.25;

        assert!(BorderKey::from_settings(&hidden).is_some());
        assert!(!build_generation_border_vertices(&hidden).is_empty());
        assert!(BorderKey::from_settings(&visible_track).is_none());
        assert!(build_generation_border_vertices(&visible_track).is_empty());
    }

    #[test]
    fn generation_radius_border_uses_board_specific_geometry() {
        let square = EngineSettings {
            board: BoardKind::LatticeSquare,
            radius: 3.9,
            track_opacity: 0.0,
            ..EngineSettings::default()
        };
        let square_vertices = build_generation_border_vertices(&square);
        assert_eq!(square_vertices.len() / FLOATS_PER_VERTEX, 5);
        assert_eq!(&square_vertices[0..2], &[-3.0, -3.0]);

        let hex = EngineSettings {
            board: BoardKind::LatticeHex,
            radius: 3.0,
            track_opacity: 0.0,
            ..EngineSettings::default()
        };
        let hex_vertices = build_generation_border_vertices(&hex);
        assert_eq!(hex_vertices.len() / FLOATS_PER_VERTEX, 7);
        assert_eq!(
            &hex_vertices[0..FLOATS_PER_VERTEX],
            &hex_vertices[hex_vertices.len() - FLOATS_PER_VERTEX..]
        );

        let triangle = EngineSettings {
            board: BoardKind::LatticeTriangle,
            radius: 3.0,
            track_opacity: 0.0,
            ..EngineSettings::default()
        };
        let triangle_vertices = build_generation_border_vertices(&triangle);
        assert!(triangle_vertices.len() / FLOATS_PER_VERTEX > 6);
        assert_eq!(
            &triangle_vertices[0..FLOATS_PER_VERTEX],
            &triangle_vertices[triangle_vertices.len() - FLOATS_PER_VERTEX..]
        );

        let continuous = EngineSettings {
            board: BoardKind::ContinuousArchimedean,
            radius: 3.25,
            track_opacity: 0.0,
            ..EngineSettings::default()
        };
        let continuous_vertices = build_generation_border_vertices(&continuous);
        let first = &continuous_vertices[0..FLOATS_PER_VERTEX];
        let last = &continuous_vertices[continuous_vertices.len() - FLOATS_PER_VERTEX..];
        assert!(((first[0] as f64).hypot(first[1] as f64) - continuous.radius).abs() < 1.0e-6);
        assert!((first[0] - last[0]).abs() < 1.0e-6);
        assert!((first[1] - last[1]).abs() < 1.0e-6);
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
