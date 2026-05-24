use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{
    Blob, BlobPropertyBag, Document, DragEvent, Element, Event, HtmlAnchorElement,
    HtmlButtonElement, HtmlElement, HtmlInputElement, HtmlSelectElement, KeyboardEvent,
    MessageEvent, MouseEvent, Url, WheelEvent, Worker,
};

use crate::protocol::{
    AppToWorker, ArmyPreset, AttackOverlayUpdate, BoardKind, CustomPiece, DEFAULT_RADIUS,
    DisplayMode, EnemyMode, EngineSettings, EngineStats, Placement, PlacementSearchMode, ShapeKind,
    SpeedMode, SpotCoord, VertexBufferUpdate, WorkerToApp, normalize_prime_modulo_divisor,
    rainbow_color,
};
use crate::render::{CanvasRenderer, ExportKind};

const RADIUS_COMMIT_DELAY_MS: i32 = 2_000;

struct AppState {
    worker: Worker,
    worker_generation: u64,
    worker_epoch: u64,
    worker_ready: bool,
    worker_initialized: bool,
    start_after_worker_ready: bool,
    step_after_worker_ready: Option<u32>,
    allow_default_auto_start: bool,
    renderer: CanvasRenderer,
    settings: EngineSettings,
    worker_settings: EngineSettings,
    visible_settings: EngineSettings,
    last_stats: EngineStats,
    running: bool,
    has_run: bool,
    dragging_army_index: Option<usize>,
    random_pool: Vec<RandomPoolPiece>,
    random_pool_editing: bool,
    first_log_lines: Vec<String>,
    recent_log_lines: Vec<String>,
    total_logged: u64,
    last_ui_refresh_ms: f64,
    render_scheduled: bool,
    run_tick_generation: u64,
    snapshot_stale: bool,
    generation_staged: bool,
    preserve_next_empty_worker_reset: bool,
    radius_commit_pending: bool,
    radius_commit_generation: u64,
    canvas_dragging: bool,
    canvas_last_x: f64,
    canvas_last_y: f64,
    board_select_pointer_value: Option<String>,
    preferred_square_shape: ShapeKind,
    preferred_hex_shape: ShapeKind,
    preferred_triangle_shape: ShapeKind,
    active_export_cancel: Option<Rc<Cell<bool>>>,
    active_export_button_id: Option<String>,
    active_export_button_text: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RandomPoolPiece {
    name: String,
    a: i32,
    b: i32,
}

#[derive(Clone)]
enum SyncAction {
    RenderOnly,
    UpdateWorker,
    ResetWorker,
    DebounceRadius,
    AutoControl(String),
}

#[derive(Clone, Copy)]
struct ImageExportButtonConfig {
    id: &'static str,
    artifact: &'static str,
    extension: &'static str,
    mime_type: &'static str,
    kind: ExportKind,
    encoder_quality: Option<f64>,
}

pub fn boot_app() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();

    let document = current_document()?;
    let worker = Worker::new(&worker_script_url(&document)?)?;
    let renderer = CanvasRenderer::new("sim-canvas")?;
    let default_settings = EngineSettings::default();
    let settings = read_settings(
        &document,
        default_settings.custom_army.clone(),
        &default_settings,
        true,
    )?;

    let state = Rc::new(RefCell::new(AppState {
        worker,
        worker_generation: 0,
        worker_epoch: 0,
        worker_ready: false,
        worker_initialized: false,
        start_after_worker_ready: false,
        step_after_worker_ready: None,
        allow_default_auto_start: true,
        renderer,
        worker_settings: settings.clone(),
        visible_settings: settings.clone(),
        settings,
        last_stats: EngineStats::default(),
        running: false,
        has_run: false,
        dragging_army_index: None,
        random_pool: default_random_pool(),
        random_pool_editing: false,
        first_log_lines: Vec::new(),
        recent_log_lines: Vec::new(),
        total_logged: 0,
        last_ui_refresh_ms: 0.0,
        render_scheduled: false,
        run_tick_generation: 0,
        snapshot_stale: false,
        generation_staged: false,
        preserve_next_empty_worker_reset: false,
        radius_commit_pending: false,
        radius_commit_generation: 0,
        canvas_dragging: false,
        canvas_last_x: 0.0,
        canvas_last_y: 0.0,
        board_select_pointer_value: None,
        preferred_square_shape: ShapeKind::Square,
        preferred_hex_shape: ShapeKind::Hex,
        preferred_triangle_shape: ShapeKind::Triangle,
        active_export_cancel: None,
        active_export_button_id: None,
        active_export_button_text: None,
    }));

    install_worker_handler(Rc::clone(&state))?;
    install_resize_handler(Rc::clone(&state))?;
    install_control_handlers(&document, Rc::clone(&state))?;
    install_canvas_interaction_handlers(&document, Rc::clone(&state))?;
    install_panel_toggle_handler(&document)?;
    render_army_list(&document, Rc::clone(&state))?;
    update_outputs(&document, &state.borrow().settings)?;
    update_run_buttons(&document, state.borrow().running)?;
    let settings = state.borrow().settings.clone();
    state.borrow_mut().renderer.set_settings(settings)?;
    set_status("Loading worker")?;

    Ok(())
}

fn install_worker_handler(state: Rc<RefCell<AppState>>) -> Result<(), JsValue> {
    let (worker, generation) = {
        let state = state.borrow();
        (state.worker.clone(), state.worker_generation)
    };
    install_worker_handler_on(&worker, state, generation)
}

fn install_worker_handler_on(
    worker: &Worker,
    state: Rc<RefCell<AppState>>,
    generation: u64,
) -> Result<(), JsValue> {
    let callback_state = Rc::clone(&state);
    let closure = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        if callback_state.borrow().worker_generation != generation {
            return;
        }

        let msg = decode_worker_message(event);
        match msg {
            Ok(WorkerToApp::Ready) => {
                if let Ok(document) = current_document() {
                    let (
                        worker,
                        epoch,
                        settings,
                        needs_initialize,
                        start_after_ready,
                        step_after_ready,
                        allow_auto_start,
                    ) = {
                        let mut state = callback_state.borrow_mut();
                        state.worker_ready = true;
                        let needs_initialize = !state.worker_initialized;
                        state.worker_initialized = true;
                        let start_after_ready = state.start_after_worker_ready;
                        let step_after_ready = state.step_after_worker_ready.take();
                        state.start_after_worker_ready = false;
                        (
                            state.worker.clone(),
                            state.worker_epoch,
                            state.worker_settings.clone(),
                            needs_initialize,
                            start_after_ready,
                            step_after_ready,
                            state.allow_default_auto_start,
                        )
                    };

                    if !needs_initialize && !start_after_ready && step_after_ready.is_none() {
                        return;
                    }

                    if needs_initialize
                        && let Err(error) =
                            send_to_worker(&worker, &AppToWorker::Initialize { epoch, settings })
                    {
                        log_error(&error);
                    }

                    if start_after_ready {
                        if let Err(error) = send_to_worker(&worker, &AppToWorker::Start { epoch }) {
                            log_error(&error);
                        }
                        if let Err(error) = send_to_worker(&worker, &AppToWorker::RunTick { epoch })
                        {
                            log_error(&error);
                        }
                        if let Err(error) =
                            set_status(&running_status_text(&callback_state.borrow().settings))
                        {
                            log_error(&error);
                        }
                    } else if let Some(max_steps) = step_after_ready {
                        if let Err(error) =
                            send_to_worker(&worker, &AppToWorker::StepBatch { epoch, max_steps })
                        {
                            log_error(&error);
                        }
                        if let Err(error) = set_status("Stepping") {
                            log_error(&error);
                        }
                    } else if allow_auto_start {
                        if let Err(error) =
                            maybe_auto_start_default(&document, Rc::clone(&callback_state))
                        {
                            log_error(&error);
                        }
                    } else {
                        if let Err(error) = update_run_buttons(&document, false) {
                            log_error(&error);
                        }
                        if let Err(error) = set_status("Worker ready") {
                            log_error(&error);
                        }
                    }
                }
            }
            Ok(WorkerToApp::Batch {
                epoch,
                log_placements,
                vertex_update,
                attack_overlay_update,
                attack_overlay_pending,
                stats,
                color_state,
            }) => {
                if callback_state.borrow().worker_epoch != epoch {
                    return;
                }
                let exhausted = stats.exhausted;
                let (status, deferred_actions) = {
                    let mut state = callback_state.borrow_mut();
                    let deferred_actions = mark_worker_ready_from_current_response(&mut state);
                    if state.generation_staged
                        || state.preserve_next_empty_worker_reset
                        || state.snapshot_stale
                    {
                        if let Err(error) = state.renderer.clear_placements() {
                            log_error(&error);
                        }
                        clear_placement_log_state(&mut state);
                    }
                    if let Err(error) = state.renderer.apply_batch(
                        &vertex_update,
                        &attack_overlay_update,
                        color_state,
                    ) {
                        log_error(&error);
                    }
                    state.visible_settings = state.worker_settings.clone();
                    state.generation_staged = false;
                    state.preserve_next_empty_worker_reset = false;
                    if let Err(error) =
                        append_placement_log(&mut state, &log_placements, stats.placements)
                    {
                        log_error(&error);
                    }
                    state.last_stats = stats;
                    if exhausted {
                        state.running = false;
                        state.has_run = true;
                    }
                    if let Err(error) = update_renderer_snapshot(&mut state) {
                        log_error(&error);
                    }
                    if should_refresh_worker_ui(&mut state, exhausted) {
                        if let Err(error) = refresh_placement_log(&state) {
                            log_error(&error);
                        }
                        (Some(status_text(&state, stats)), deferred_actions)
                    } else {
                        (None, deferred_actions)
                    }
                };
                if let Some(status) = status
                    && let Err(error) = set_status(&status)
                {
                    log_error(&error);
                }
                if let Some((worker, epoch, start_after_ready, step_after_ready)) = deferred_actions
                    && let Err(error) = dispatch_deferred_worker_actions(
                        worker,
                        epoch,
                        start_after_ready,
                        step_after_ready,
                    )
                {
                    log_error(&error);
                }
                if let Err(error) = schedule_render(Rc::clone(&callback_state)) {
                    log_error(&error);
                }
                if attack_overlay_pending
                    && let Err(error) = request_attack_overlay_chunk(Rc::clone(&callback_state))
                {
                    log_error(&error);
                }
                if exhausted {
                    if let Ok(document) = current_document()
                        && let Err(error) =
                            update_run_buttons(&document, callback_state.borrow().running)
                    {
                        log_error(&error);
                    }
                } else if callback_state.borrow().running
                    && let Err(error) = schedule_next_run_tick(Rc::clone(&callback_state))
                {
                    log_error(&error);
                }
            }
            Ok(WorkerToApp::Stats {
                epoch,
                stats,
                color_state,
                vertex_update,
                attack_overlay_update,
                attack_overlay_pending,
            }) => {
                if callback_state.borrow().worker_epoch != epoch {
                    return;
                }
                let deferred_actions = {
                    let mut state = callback_state.borrow_mut();
                    mark_worker_ready_from_current_response(&mut state)
                };
                let preserve_empty_reset = {
                    let state = callback_state.borrow();
                    state.preserve_next_empty_worker_reset
                        && stats.placements == 0
                        && matches!(&vertex_update, VertexBufferUpdate::Replace(vertices) if vertices.is_empty())
                };
                if preserve_empty_reset {
                    let status = {
                        let mut state = callback_state.borrow_mut();
                        if let Err(error) = update_renderer_snapshot(&mut state) {
                            log_error(&error);
                        }
                        status_text(&state, stats)
                    };
                    if let Ok(document) = current_document() {
                        let state = callback_state.borrow();
                        if let Err(error) = update_run_buttons(&document, state.running) {
                            log_error(&error);
                        }
                    }
                    if let Err(error) = set_status(&status) {
                        log_error(&error);
                    }
                    if let Some((worker, epoch, start_after_ready, step_after_ready)) =
                        deferred_actions
                        && let Err(error) = dispatch_deferred_worker_actions(
                            worker,
                            epoch,
                            start_after_ready,
                            step_after_ready,
                        )
                    {
                        log_error(&error);
                    }
                    return;
                }

                let exhausted = stats.exhausted;
                let needs_render = !matches!(vertex_update, VertexBufferUpdate::None)
                    || !attack_overlay_update_is_none(&attack_overlay_update);
                let empty_replace = stats.placements == 0
                    && matches!(&vertex_update, VertexBufferUpdate::Replace(vertices) if vertices.is_empty());
                let status = {
                    let mut state = callback_state.borrow_mut();
                    if let Err(error) = state.renderer.apply_stats(
                        &vertex_update,
                        &attack_overlay_update,
                        color_state,
                    ) {
                        log_error(&error);
                    }
                    if !matches!(vertex_update, VertexBufferUpdate::None) {
                        state.visible_settings = state.worker_settings.clone();
                        state.generation_staged = false;
                    }
                    if empty_replace {
                        state.first_log_lines.clear();
                        state.recent_log_lines.clear();
                        state.total_logged = 0;
                    }
                    state.last_stats = stats;
                    if exhausted {
                        state.running = false;
                        state.has_run = true;
                    }
                    if let Err(error) = update_renderer_snapshot(&mut state) {
                        log_error(&error);
                    }
                    state.last_ui_refresh_ms = js_sys::Date::now();
                    if let Err(error) = refresh_placement_log(&state) {
                        log_error(&error);
                    }
                    status_text(&state, stats)
                };
                if let Err(error) = set_status(&status) {
                    log_error(&error);
                }
                if let Some((worker, epoch, start_after_ready, step_after_ready)) = deferred_actions
                    && let Err(error) = dispatch_deferred_worker_actions(
                        worker,
                        epoch,
                        start_after_ready,
                        step_after_ready,
                    )
                {
                    log_error(&error);
                }
                if needs_render && let Err(error) = schedule_render(Rc::clone(&callback_state)) {
                    log_error(&error);
                }
                if attack_overlay_pending
                    && let Err(error) = request_attack_overlay_chunk(Rc::clone(&callback_state))
                {
                    log_error(&error);
                }
                if exhausted {
                    if let Ok(document) = current_document()
                        && let Err(error) =
                            update_run_buttons(&document, callback_state.borrow().running)
                    {
                        log_error(&error);
                    }
                } else if callback_state.borrow().running
                    && let Err(error) = schedule_next_run_tick(Rc::clone(&callback_state))
                {
                    log_error(&error);
                }
            }
            Ok(WorkerToApp::Error { epoch, message }) => {
                if callback_state.borrow().worker_epoch != epoch {
                    return;
                }
                if let Ok(document) = current_document() {
                    let mut state = callback_state.borrow_mut();
                    state.running = false;
                    state.has_run = false;
                    if let Err(error) = update_run_buttons(&document, state.running) {
                        log_error(&error);
                    }
                }
                if let Err(error) = set_status(&format!("Worker error: {message}")) {
                    log_error(&error);
                }
            }
            Err(error) => {
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "failed to decode worker message: {error}"
                )));
            }
        }
    });

    worker.set_onmessage(Some(closure.as_ref().unchecked_ref()));
    closure.forget();
    Ok(())
}

fn append_placement_log(
    state: &mut AppState,
    placements: &[Placement],
    total_placements: u64,
) -> Result<(), JsValue> {
    let recent_start = total_placements.saturating_sub(32);

    for placement in placements {
        let line = placement_log_line(placement);

        if placement.id < 32 && placement.id as usize == state.first_log_lines.len() {
            state.first_log_lines.push(line.clone());
        }

        if placement.id >= recent_start {
            if placement.id == recent_start {
                state.recent_log_lines.clear();
            }
            state.recent_log_lines.push(line);
        }
    }

    while state.recent_log_lines.len() > 32 {
        state.recent_log_lines.remove(0);
    }
    state.total_logged = total_placements;

    Ok(())
}

fn reset_placement_log(state: &mut AppState) -> Result<(), JsValue> {
    clear_placement_log_state(state);
    refresh_placement_log(state)
}

fn clear_placement_log_state(state: &mut AppState) {
    state.first_log_lines.clear();
    state.recent_log_lines.clear();
    state.total_logged = 0;
}

fn attack_overlay_update_is_none(update: &AttackOverlayUpdate) -> bool {
    matches!(update.spots, VertexBufferUpdate::None)
        && matches!(update.hits, VertexBufferUpdate::None)
        && matches!(update.circles, VertexBufferUpdate::None)
}

fn update_renderer_snapshot(state: &mut AppState) -> Result<(), JsValue> {
    state.snapshot_stale = state.last_stats.placements > 0
        && !simulation_settings_match(&state.visible_settings, &state.settings);
    let (view_settings, placement_settings) = render_settings_for_snapshot(state);
    state
        .renderer
        .set_snapshot_settings(view_settings, placement_settings)?;
    state
        .renderer
        .set_color_saturation(if state.snapshot_stale { 0.5 } else { 1.0 })?;
    sync_generation_border_visibility(state)
}

fn sync_generation_border_visibility(state: &mut AppState) -> Result<(), JsValue> {
    state
        .renderer
        .set_generation_border_visible(!should_hide_generation_border(state))
}

fn should_hide_generation_border(state: &AppState) -> bool {
    state.last_stats.exhausted
        && !state.running
        && !state.snapshot_stale
        && !state.generation_staged
        && !state.preserve_next_empty_worker_reset
        && simulation_settings_match(&state.visible_settings, &state.settings)
        && state.last_stats.current_radius + 1.0e-9 >= state.settings.radius.max(0.0)
}

fn render_settings_for_snapshot(state: &AppState) -> (EngineSettings, EngineSettings) {
    let mut placement_settings = if state.snapshot_stale {
        state.visible_settings.clone()
    } else {
        state.settings.clone()
    };
    placement_settings.attack_overlay_opacity = state.settings.attack_overlay_opacity;

    let mut view_settings = state.settings.clone();
    view_settings.attack_overlay_opacity = state.settings.attack_overlay_opacity;
    (view_settings, placement_settings)
}

fn simulation_settings_match(visible: &EngineSettings, current: &EngineSettings) -> bool {
    visible.board == current.board
        && radius_settings_match(visible, current)
        && continuous_piece_radius_matches(visible, current)
        && visible.proactive_attacking == current.proactive_attacking
        && visible.enemy_mode == current.enemy_mode
        && visible.placement_search == current.placement_search
        && visible.army_preset == current.army_preset
        && visible.custom_army == current.custom_army
        && visible.continuous_offset == current.continuous_offset
        && visible.prime_modulo_divisor == current.prime_modulo_divisor
}

fn radius_settings_match(visible: &EngineSettings, current: &EngineSettings) -> bool {
    visible.radius == current.radius
        || (visible.placement_search == PlacementSearchMode::SpiralPath
            && current.placement_search == PlacementSearchMode::SpiralPath
            && current.radius >= visible.radius)
}

fn continuous_piece_radius_matches(visible: &EngineSettings, current: &EngineSettings) -> bool {
    if visible.board == BoardKind::ContinuousArchimedean
        || current.board == BoardKind::ContinuousArchimedean
    {
        visible.piece_radius == current.piece_radius
    } else {
        true
    }
}

fn refresh_placement_log(state: &AppState) -> Result<(), JsValue> {
    let document = current_document()?;
    let text = placement_log_text(state);
    set_text(&document, "placement-log", &text)
}

fn should_refresh_worker_ui(state: &mut AppState, force: bool) -> bool {
    const UI_REFRESH_INTERVAL_MS: f64 = 100.0;

    let now = js_sys::Date::now();
    if force || now - state.last_ui_refresh_ms >= UI_REFRESH_INTERVAL_MS {
        state.last_ui_refresh_ms = now;
        true
    } else {
        false
    }
}

fn placement_log_text(state: &AppState) -> String {
    if state.total_logged == 0 {
        return "No placements yet.".to_string();
    }

    let mut out = format!(
        "settings: board={:?} shape={:?} search={:?} army={:?} enemy={:?} attacking={} radius={:.2} piece_radius={:.2} visual_progress={} track={:.2} attacks={:.2} offset={:.3} anchors={}..{}\nplacements logged: {}\n\nfirst placements:\n",
        state.settings.board,
        state.settings.shape,
        state.settings.placement_search,
        state.settings.army_preset,
        state.settings.enemy_mode,
        state.settings.proactive_attacking,
        state.settings.radius,
        state.settings.piece_radius,
        state.settings.visual_progress,
        state.settings.track_opacity,
        state.settings.attack_overlay_opacity,
        state.settings.continuous_offset,
        state.settings.anchor_color_a,
        state.settings.anchor_color_b,
        state.total_logged
    );

    for line in &state.first_log_lines {
        out.push_str(line);
        out.push('\n');
    }

    if state.total_logged as usize > state.first_log_lines.len() {
        out.push_str("\nlatest placements:\n");
        for line in &state.recent_log_lines {
            out.push_str(line);
            out.push('\n');
        }
    }

    out
}

fn placement_log_line(placement: &Placement) -> String {
    format!(
        "#{:06} spot={} coord={} piece=({}, {}) color_group={} color_t={:.6} rule={:?}",
        placement.id,
        placement.spot_index,
        spot_coord_text(placement.coord),
        placement.piece.a,
        placement.piece.b,
        placement.color.key.group,
        placement.color.key.gradient_value,
        placement.color.rule
    )
}

fn spot_coord_text(coord: SpotCoord) -> String {
    match coord {
        SpotCoord::Square { x, y } => format!("square({x},{y})"),
        SpotCoord::Hex { q, r } => format!("hex({q},{r})"),
        SpotCoord::Triangle { u, v } => format!("triangle({u},{v})"),
        SpotCoord::Continuous { x, y, theta } => {
            format!("continuous(x={x:.9},y={y:.9},theta={theta:.9})")
        }
    }
}

fn download_log(state: &mut AppState) -> Result<(), JsValue> {
    let text = placement_log_text(state);
    let parts = js_sys::Array::new();
    parts.push(&JsValue::from_str(&text));

    let options = BlobPropertyBag::new();
    options.set_type("text/plain;charset=utf-8");
    let blob = Blob::new_with_str_sequence_and_options(&parts, &options)?;
    let url = Url::create_object_url_with_blob(&blob)?;
    let document = current_document()?;
    let anchor = document
        .create_element("a")?
        .dyn_into::<HtmlAnchorElement>()?;
    anchor.set_href(&url);
    anchor.set_download(&download_filename(
        &state.settings,
        state.last_stats,
        "placement-log",
        "txt",
    ));
    anchor.click();
    Url::revoke_object_url(&url)?;
    Ok(())
}

fn download_filename(
    settings: &EngineSettings,
    stats: EngineStats,
    artifact: &str,
    extension: &str,
) -> String {
    let status = if stats.exhausted {
        "complete"
    } else {
        "partial"
    };
    let attacking = if settings.proactive_attacking {
        "attacking-on"
    } else {
        "attacking-off"
    };

    format!(
        "spiral-{artifact}-{board}-{army}-{enemy}-{shape}-r{radius}-pr{piece_radius}-{attacking}-{status}-{placements}p.{extension}",
        board = slug(&format!("{:?}", settings.board)),
        army = slug(&format!("{:?}", settings.army_preset)),
        enemy = slug(&format!("{:?}", settings.enemy_mode)),
        shape = slug(&format!("{:?}", settings.shape)),
        radius = number_token(settings.radius, 2),
        piece_radius = number_token(settings.piece_radius, 2),
        placements = stats.placements,
    )
}

fn slug(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

fn number_token(value: f64, decimals: usize) -> String {
    let text = format!("{value:.prec$}", prec = decimals);
    text.trim_end_matches('0')
        .trim_end_matches('.')
        .replace('-', "m")
        .replace('.', "p")
}

fn schedule_next_run_tick(state: Rc<RefCell<AppState>>) -> Result<(), JsValue> {
    let (delay_ms, generation) = {
        let mut state = state.borrow_mut();
        state.run_tick_generation = state.run_tick_generation.wrapping_add(1);
        (
            run_delay_ms(&state.settings.speed),
            state.run_tick_generation,
        )
    };
    let closure = Closure::<dyn FnMut()>::new(move || {
        let state = state.borrow();
        if state.run_tick_generation != generation {
            return;
        }
        if state.running
            && state.worker_ready
            && let Err(error) = send_to_worker(
                &state.worker,
                &AppToWorker::RunTick {
                    epoch: state.worker_epoch,
                },
            )
        {
            log_error(&error);
        }
    });

    web_sys::window()
        .ok_or_else(|| JsValue::from_str("window unavailable"))?
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            delay_ms,
        )?;
    closure.forget();
    Ok(())
}

fn request_attack_overlay_chunk(state: Rc<RefCell<AppState>>) -> Result<(), JsValue> {
    let state = state.borrow();
    if !state.worker_ready {
        return Ok(());
    }
    send_to_worker(
        &state.worker,
        &AppToWorker::BuildAttackOverlay {
            epoch: state.worker_epoch,
        },
    )
}

fn schedule_step_batch(worker: Worker, epoch: u64) -> Result<(), JsValue> {
    let closure = Closure::<dyn FnMut()>::new(move || {
        if let Err(error) = send_to_worker(
            &worker,
            &AppToWorker::StepBatch {
                epoch,
                max_steps: 1,
            },
        ) {
            log_error(&error);
        }
    });

    web_sys::window()
        .ok_or_else(|| JsValue::from_str("window unavailable"))?
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            500,
        )?;
    closure.forget();
    Ok(())
}

fn run_delay_ms(speed: &SpeedMode) -> i32 {
    match speed {
        SpeedMode::Fastest => 0,
        SpeedMode::PerSecond(rate) => {
            let rate = (*rate).max(1) as u32;
            let batch = if rate <= 20 { 1 } else { rate.div_ceil(20) };
            ((1000.0 * batch as f64) / rate as f64).round().max(1.0) as i32
        }
    }
}

fn schedule_render(state: Rc<RefCell<AppState>>) -> Result<(), JsValue> {
    {
        let mut state = state.borrow_mut();
        if state.render_scheduled {
            return Ok(());
        }
        state.render_scheduled = true;
    }

    let closure = Closure::<dyn FnMut(f64)>::new(move |_timestamp: f64| {
        let mut state = state.borrow_mut();
        state.render_scheduled = false;
        if let Err(error) = state.renderer.redraw() {
            log_error(&error);
        }
    });

    web_sys::window()
        .ok_or_else(|| JsValue::from_str("window unavailable"))?
        .request_animation_frame(closure.as_ref().unchecked_ref())?;
    closure.forget();
    Ok(())
}

fn install_resize_handler(state: Rc<RefCell<AppState>>) -> Result<(), JsValue> {
    let closure = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        if let Err(error) = state.borrow_mut().renderer.resize_to_viewport() {
            log_error(&error);
        }
    });

    web_sys::window()
        .ok_or_else(|| JsValue::from_str("window unavailable"))?
        .add_event_listener_with_callback("resize", closure.as_ref().unchecked_ref())?;
    closure.forget();
    Ok(())
}

fn install_canvas_interaction_handlers(
    document: &Document,
    state: Rc<RefCell<AppState>>,
) -> Result<(), JsValue> {
    let canvas = document
        .get_element_by_id("sim-canvas")
        .ok_or_else(|| JsValue::from_str("missing canvas"))?;

    let down_state = Rc::clone(&state);
    let down_canvas = canvas.clone();
    let mouse_down = Closure::<dyn FnMut(MouseEvent)>::new(move |event: MouseEvent| {
        if event.button() != 0 {
            return;
        }
        let mut state = down_state.borrow_mut();
        if !canvas_pan_enabled(&state.settings) {
            return;
        }
        event.prevent_default();
        state.canvas_dragging = true;
        state.canvas_last_x = event.client_x() as f64;
        state.canvas_last_y = event.client_y() as f64;
        if let Err(error) = down_canvas.class_list().add_1("dragging") {
            log_error(&error);
        }
    });
    canvas.add_event_listener_with_callback("mousedown", mouse_down.as_ref().unchecked_ref())?;
    mouse_down.forget();

    let move_state = Rc::clone(&state);
    let mouse_move = Closure::<dyn FnMut(MouseEvent)>::new(move |event: MouseEvent| {
        let mut state = move_state.borrow_mut();
        if !state.canvas_dragging {
            return;
        }
        let x = event.client_x() as f64;
        let y = event.client_y() as f64;
        let dx = x - state.canvas_last_x;
        let dy = y - state.canvas_last_y;
        state.canvas_last_x = x;
        state.canvas_last_y = y;
        if let Err(error) = state.renderer.pan_by_pixels(dx, dy) {
            log_error(&error);
        }
    });
    web_sys::window()
        .ok_or_else(|| JsValue::from_str("window unavailable"))?
        .add_event_listener_with_callback("mousemove", mouse_move.as_ref().unchecked_ref())?;
    mouse_move.forget();

    let up_state = Rc::clone(&state);
    let up_canvas = canvas.clone();
    let mouse_up = Closure::<dyn FnMut(MouseEvent)>::new(move |_event: MouseEvent| {
        up_state.borrow_mut().canvas_dragging = false;
        if let Err(error) = up_canvas.class_list().remove_1("dragging") {
            log_error(&error);
        }
    });
    web_sys::window()
        .ok_or_else(|| JsValue::from_str("window unavailable"))?
        .add_event_listener_with_callback("mouseup", mouse_up.as_ref().unchecked_ref())?;
    mouse_up.forget();

    let wheel_document = document.clone();
    let wheel_state = Rc::clone(&state);
    let wheel = Closure::<dyn FnMut(WheelEvent)>::new(move |event: WheelEvent| {
        let mut state = wheel_state.borrow_mut();
        if state.settings.display_mode == DisplayMode::FitScreen {
            state.settings.display_mode = DisplayMode::PixelOneToOne;
            if let Err(error) =
                set_select_value(&wheel_document, "display-mode-select", "PixelOneToOne")
            {
                log_error(&error);
            }
            if let Err(error) = update_outputs(&wheel_document, &state.settings) {
                log_error(&error);
            }
            if let Err(error) = update_renderer_snapshot(&mut state) {
                log_error(&error);
            }
        }
        if state.settings.display_mode != DisplayMode::PixelOneToOne {
            return;
        }
        event.prevent_default();
        let delta = if event.delta_y() < 0.0 { 1 } else { -1 };
        match state
            .renderer
            .zoom_at(event.client_x() as f64, event.client_y() as f64, delta)
        {
            Ok(zoom) => {
                state.settings.zoom = zoom;
                if let Ok(input) = input(&wheel_document, "zoom-slider") {
                    input.set_value(&zoom.to_string());
                }
                if let Err(error) = update_outputs(&wheel_document, &state.settings) {
                    log_error(&error);
                }
            }
            Err(error) => log_error(&error),
        }
    });
    canvas.add_event_listener_with_callback("wheel", wheel.as_ref().unchecked_ref())?;
    wheel.forget();

    Ok(())
}

fn install_panel_toggle_handler(document: &Document) -> Result<(), JsValue> {
    let panel = html_element(document, "control-panel")?;
    let button = button(document, "panel-toggle-button")?;
    let toggle_button = button.clone();
    let closure = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        let collapsed = panel.class_list().toggle("collapsed").unwrap_or(false);
        toggle_button.set_text_content(Some(if collapsed { "Show" } else { "Hide" }));
        toggle_button.set_title(if collapsed {
            "Show controls"
        } else {
            "Collapse controls"
        });
    });

    button.add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())?;
    closure.forget();
    Ok(())
}

fn install_control_handlers(
    document: &Document,
    state: Rc<RefCell<AppState>>,
) -> Result<(), JsValue> {
    for id in [
        "board-select",
        "continuous-offset-input",
        "attacking-toggle",
        "enemy-mode-select",
        "placement-search-select",
        "army-preset-select",
    ] {
        install_settings_handler(document, id, Rc::clone(&state), SyncAction::ResetWorker)?;
    }

    for id in ["shape-select", "piece-radius-slider", "radius-input"] {
        install_settings_handler(
            document,
            id,
            Rc::clone(&state),
            SyncAction::AutoControl(id.to_string()),
        )?;
    }

    for id in ["speed-slider", "fastest-toggle"] {
        install_settings_handler(document, id, Rc::clone(&state), SyncAction::UpdateWorker)?;
    }
    install_settings_handler(
        document,
        "visual-progress-toggle",
        Rc::clone(&state),
        SyncAction::AutoControl("visual-progress-toggle".to_string()),
    )?;

    for id in ["display-mode-select", "zoom-slider", "track-opacity-slider"] {
        install_settings_handler(document, id, Rc::clone(&state), SyncAction::RenderOnly)?;
    }
    install_settings_handler(
        document,
        "attack-overlay-opacity-slider",
        Rc::clone(&state),
        SyncAction::UpdateWorker,
    )?;

    for id in ["anchor-a-input", "anchor-b-input"] {
        install_settings_handler(document, id, Rc::clone(&state), SyncAction::UpdateWorker)?;
    }
    install_same_board_refresh_handler(document, Rc::clone(&state))?;
    install_select_quick_close_handlers(document, Rc::clone(&state))?;
    install_continuous_offset_blur_handler(document, Rc::clone(&state))?;
    install_prime_divisor_commit_handlers(document, Rc::clone(&state))?;

    install_add_piece_handler(document, Rc::clone(&state))?;
    install_random_piece_handler(document, Rc::clone(&state))?;
    install_random_pool_toggle_handler(document, Rc::clone(&state))?;
    install_button(document, "start-button", Rc::clone(&state), |state| {
        let document = current_document()?;
        sync_state_from_controls(&document, state, true)?;
        commit_pending_radius_change(state, &document, false, true)?;
        if state.running {
            return Ok(());
        }
        prepare_new_generation_if_staged(state, &document)?;
        state.run_tick_generation = state.run_tick_generation.wrapping_add(1);
        state.running = true;
        state.has_run = true;
        update_run_buttons(&document, state.running)?;
        set_status(&running_status_text(&state.settings))?;
        if !state.worker_ready {
            state.start_after_worker_ready = true;
            state.step_after_worker_ready = None;
            return Ok(());
        }
        ensure_worker_initialized(state)?;
        let epoch = state.worker_epoch;
        send_to_worker(&state.worker, &AppToWorker::Start { epoch })?;
        send_to_worker(&state.worker, &AppToWorker::RunTick { epoch })
    })?;
    install_button(document, "pause-button", Rc::clone(&state), |state| {
        let document = current_document()?;
        sync_state_from_controls(&document, state, true)?;
        commit_pending_radius_change(state, &document, false, false)?;
        if !state.running {
            return Ok(());
        }
        state.run_tick_generation = state.run_tick_generation.wrapping_add(1);
        state.running = false;
        state.has_run = true;
        update_run_buttons(&document, state.running)?;
        set_status("Paused")?;
        send_to_worker(
            &state.worker,
            &AppToWorker::Pause {
                epoch: state.worker_epoch,
            },
        )
    })?;
    install_button(document, "step-button", Rc::clone(&state), |state| {
        let document = current_document()?;
        sync_state_from_controls(&document, state, true)?;
        commit_pending_radius_change(state, &document, false, true)?;
        if state.running {
            return Ok(());
        }
        prepare_new_generation_if_staged(state, &document)?;
        if !state.worker_ready {
            state.step_after_worker_ready = Some(1);
            state.has_run = true;
            set_status("Stepping")?;
            return Ok(());
        }
        ensure_worker_initialized(state)?;
        if state.last_stats.placements == 0 {
            let settings = state.settings.clone();
            reset_worker_with_settings(state, &document, settings, false, true)?;
            set_status("Stepping")?;
            return schedule_step_batch(state.worker.clone(), state.worker_epoch);
        }
        set_status("Stepping")?;
        send_to_worker(
            &state.worker,
            &AppToWorker::StepBatch {
                epoch: state.worker_epoch,
                max_steps: 1,
            },
        )
    })?;
    install_refresh_handler(document, Rc::clone(&state))?;
    install_image_export_button(
        document,
        Rc::clone(&state),
        ImageExportButtonConfig {
            id: "download-full-png-button",
            artifact: "image-full",
            extension: "png",
            mime_type: "image/png",
            kind: ExportKind::FullPng,
            encoder_quality: None,
        },
    )?;
    install_image_export_button(
        document,
        Rc::clone(&state),
        ImageExportButtonConfig {
            id: "download-png-button",
            artifact: "image",
            extension: "png",
            mime_type: "image/png",
            kind: ExportKind::Png,
            encoder_quality: None,
        },
    )?;
    install_image_export_button(
        document,
        Rc::clone(&state),
        ImageExportButtonConfig {
            id: "download-jpeg-button",
            artifact: "image-half",
            extension: "jpg",
            mime_type: "image/jpeg",
            kind: ExportKind::JpegHalf,
            encoder_quality: Some(0.82),
        },
    )?;
    install_button(document, "download-log-button", state, |state| {
        download_log(state)
    })?;

    Ok(())
}

fn install_settings_handler(
    document: &Document,
    id: &str,
    state: Rc<RefCell<AppState>>,
    action: SyncAction,
) -> Result<(), JsValue> {
    let document = document.clone();
    let element = document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("missing control #{id}")))?;
    let closure = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        if let Err(error) = sync_settings(&document, Rc::clone(&state), action.clone(), false) {
            log_error(&error);
        }
    });

    if element.dyn_ref::<HtmlSelectElement>().is_some() {
        element.add_event_listener_with_callback("change", closure.as_ref().unchecked_ref())?;
    } else if let Some(input) = element.dyn_ref::<HtmlInputElement>() {
        if input.type_() == "checkbox" {
            element.add_event_listener_with_callback("change", closure.as_ref().unchecked_ref())?;
        } else {
            element.add_event_listener_with_callback("input", closure.as_ref().unchecked_ref())?;
        }
    } else {
        element.add_event_listener_with_callback("change", closure.as_ref().unchecked_ref())?;
    }
    closure.forget();
    Ok(())
}

fn install_continuous_offset_blur_handler(
    document: &Document,
    state: Rc<RefCell<AppState>>,
) -> Result<(), JsValue> {
    let document = document.clone();
    let offset_input = input(&document, "continuous-offset-input")?;
    let closure = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        let raw = match input_value(&document, "continuous-offset-input") {
            Ok(value) => value,
            Err(error) => {
                log_error(&error);
                return;
            }
        };
        if validate_continuous_offset_text(&raw).0 {
            return;
        }

        let fallback = if raw.trim().is_empty() {
            0.0
        } else {
            state.borrow().settings.continuous_offset
        };
        match input(&document, "continuous-offset-input") {
            Ok(input) => input.set_value(&continuous_offset_value_text(fallback)),
            Err(error) => {
                log_error(&error);
                return;
            }
        }

        if let Err(error) =
            sync_settings(&document, Rc::clone(&state), SyncAction::ResetWorker, false)
        {
            log_error(&error);
        }
    });

    offset_input.add_event_listener_with_callback("blur", closure.as_ref().unchecked_ref())?;
    closure.forget();
    Ok(())
}

fn install_prime_divisor_commit_handlers(
    document: &Document,
    state: Rc<RefCell<AppState>>,
) -> Result<(), JsValue> {
    let divisor_input = input(document, "prime-divisor-input")?;

    let blur_document = document.clone();
    let blur_state = Rc::clone(&state);
    let blur = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        if let Err(error) = commit_prime_modulo_divisor(&blur_document, Rc::clone(&blur_state)) {
            log_error(&error);
        }
    });
    divisor_input.add_event_listener_with_callback("blur", blur.as_ref().unchecked_ref())?;
    divisor_input.add_event_listener_with_callback("change", blur.as_ref().unchecked_ref())?;
    blur.forget();

    let key_document = document.clone();
    let key_state = state;
    let keydown = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
        if event.key() != "Enter" {
            return;
        }
        event.prevent_default();
        if let Err(error) = commit_prime_modulo_divisor(&key_document, Rc::clone(&key_state)) {
            log_error(&error);
        }
    });
    divisor_input.add_event_listener_with_callback("keydown", keydown.as_ref().unchecked_ref())?;
    keydown.forget();
    Ok(())
}

fn commit_prime_modulo_divisor(
    document: &Document,
    state: Rc<RefCell<AppState>>,
) -> Result<(), JsValue> {
    let current = state.borrow().settings.prime_modulo_divisor;
    let raw = input_value(document, "prime-divisor-input")?;
    let normalized = raw
        .trim()
        .parse::<u32>()
        .ok()
        .map(normalize_prime_modulo_divisor)
        .unwrap_or(current);
    input(document, "prime-divisor-input")?.set_value(&normalized.to_string());

    if normalized == current {
        return Ok(());
    }

    sync_settings(document, state, SyncAction::ResetWorker, true)
}

fn install_same_board_refresh_handler(
    document: &Document,
    state: Rc<RefCell<AppState>>,
) -> Result<(), JsValue> {
    let board_select = select(document, "board-select")?;

    let down_state = Rc::clone(&state);
    let down_select = board_select.clone();
    let mouse_down = Closure::<dyn FnMut(MouseEvent)>::new(move |_event: MouseEvent| {
        down_state.borrow_mut().board_select_pointer_value = Some(down_select.value());
    });
    board_select
        .add_event_listener_with_callback("mousedown", mouse_down.as_ref().unchecked_ref())?;
    mouse_down.forget();

    let change_state = Rc::clone(&state);
    let change = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        change_state.borrow_mut().board_select_pointer_value = None;
    });
    board_select.add_event_listener_with_callback("change", change.as_ref().unchecked_ref())?;
    change.forget();

    let blur_document = document.clone();
    let blur_state = Rc::clone(&state);
    let blur_select = board_select.clone();
    let blur = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let related_target =
            js_sys::Reflect::get(event.as_ref(), &JsValue::from_str("relatedTarget"))
                .unwrap_or(JsValue::NULL);
        let moved_to_another_control = is_form_control_blur_target(&related_target);
        let timeout_document = blur_document.clone();
        let timeout_state = Rc::clone(&blur_state);
        let timeout_select = blur_select.clone();
        let timeout = Closure::<dyn FnMut()>::new(move || {
            let should_refresh = {
                let mut state = timeout_state.borrow_mut();
                let previous = state.board_select_pointer_value.take();
                let active_control = timeout_document.active_element().is_some_and(|element| {
                    element.get_attribute("id").as_deref() != Some("board-select")
                        && is_form_control_element(&element)
                });
                !moved_to_another_control
                    && !active_control
                    && previous.as_deref() == Some(timeout_select.value().as_str())
            };
            if !should_refresh {
                return;
            }

            let settings = {
                let mut state = timeout_state.borrow_mut();
                if let Err(error) = sync_state_from_controls(&timeout_document, &mut state, true) {
                    log_error(&error);
                    return;
                }
                state.settings.clone()
            };
            if let Err(error) = recreate_worker_with_settings(
                &timeout_document,
                Rc::clone(&timeout_state),
                settings,
                false,
                true,
                "Refreshed | Paused",
            ) {
                log_error(&error);
            }
        });

        if let Err(error) = web_sys::window()
            .ok_or_else(|| JsValue::from_str("window unavailable"))
            .and_then(|window| {
                window
                    .set_timeout_with_callback_and_timeout_and_arguments_0(
                        timeout.as_ref().unchecked_ref(),
                        0,
                    )
                    .map(|_| ())
            })
        {
            log_error(&error);
        }
        timeout.forget();
    });
    board_select.add_event_listener_with_callback("blur", blur.as_ref().unchecked_ref())?;
    blur.forget();

    Ok(())
}

fn install_select_quick_close_handlers(
    document: &Document,
    state: Rc<RefCell<AppState>>,
) -> Result<(), JsValue> {
    let recent_select = Rc::new(RefCell::new(None::<(String, f64)>));
    for select_id in [
        "board-select",
        "shape-select",
        "display-mode-select",
        "enemy-mode-select",
        "placement-search-select",
        "army-preset-select",
    ] {
        let select = select(document, select_id)?;
        let id = select.id();
        let state = Rc::clone(&state);
        let pointer_recent_select = Rc::clone(&recent_select);
        let closure_select = select.clone();
        let pointer_closure = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let now = js_sys::Date::now();
            let recent_same_select = pointer_recent_select
                .borrow()
                .as_ref()
                .is_some_and(|(recent_id, time)| recent_id == &id && now - *time <= 1_000.0);

            if recent_same_select {
                event.prevent_default();
                event.stop_propagation();
                if id == "board-select" {
                    state.borrow_mut().board_select_pointer_value = None;
                }
                if let Err(error) = closure_select.blur() {
                    log_error(&error);
                }
                *pointer_recent_select.borrow_mut() = None;
            } else {
                *pointer_recent_select.borrow_mut() = Some((id.clone(), now));
            }
        });
        select.add_event_listener_with_callback_and_bool(
            "pointerdown",
            pointer_closure.as_ref().unchecked_ref(),
            true,
        )?;
        pointer_closure.forget();

        let id = select.id();
        let focus_recent_select = Rc::clone(&recent_select);
        let focus_closure = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
            *focus_recent_select.borrow_mut() = Some((id.clone(), js_sys::Date::now()));
        });
        select.add_event_listener_with_callback("focus", focus_closure.as_ref().unchecked_ref())?;
        focus_closure.forget();

        let change_recent_select = Rc::clone(&recent_select);
        let change_closure = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
            *change_recent_select.borrow_mut() = None;
        });
        select
            .add_event_listener_with_callback("change", change_closure.as_ref().unchecked_ref())?;
        change_closure.forget();
    }

    Ok(())
}

fn is_form_control_blur_target(value: &JsValue) -> bool {
    let Some(element) = value.dyn_ref::<Element>() else {
        return false;
    };

    is_form_control_element(element)
}

fn is_form_control_element(element: &Element) -> bool {
    matches!(
        element.tag_name().as_str(),
        "BUTTON" | "INPUT" | "SELECT" | "TEXTAREA"
    )
}

fn install_add_piece_handler(
    document: &Document,
    state: Rc<RefCell<AppState>>,
) -> Result<(), JsValue> {
    let document = document.clone();
    let button = document
        .get_element_by_id("add-piece-button")
        .ok_or_else(|| JsValue::from_str("missing add-piece-button"))?
        .dyn_into::<HtmlButtonElement>()?;
    let closure = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        let a = input_value(&document, "piece-a-input")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2);
        let b = input_value(&document, "piece-b-input")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        let piece = CustomPiece::with_auto_color(a, b);

        if state.borrow().random_pool_editing {
            {
                let mut state = state.borrow_mut();
                state.random_pool.push(RandomPoolPiece {
                    name: random_pool_piece_name(a, b)
                        .map_or_else(|| format!("Custom ({a}, {b})"), str::to_string),
                    a,
                    b,
                });
            }
            if let Err(error) = render_army_list(&document, Rc::clone(&state)) {
                log_error(&error);
            }
            return;
        }

        let previous_settings = {
            let mut state = state.borrow_mut();
            let previous_settings = state.settings.clone();
            state.settings.custom_army.push(piece);
            previous_settings
        };

        if let Err(error) = sync_settings_with_previous(
            &document,
            Rc::clone(&state),
            SyncAction::ResetWorker,
            previous_settings,
            false,
        ) {
            log_error(&error);
        }
    });

    button.add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())?;
    closure.forget();
    Ok(())
}

fn install_random_piece_handler(
    document: &Document,
    state: Rc<RefCell<AppState>>,
) -> Result<(), JsValue> {
    let document = document.clone();
    let button = button(&document, "random-piece-button")?;
    let closure = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        let count = input_value(&document, "random-count-input")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(3)
            .min(10_000);

        let previous_settings = {
            let mut state = state.borrow_mut();
            if state.settings.army_preset != ArmyPreset::CustomFinite {
                return;
            }
            if let Err(error) = sync_state_from_controls(&document, &mut state, true) {
                log_error(&error);
                return;
            }
            let previous_settings = state.settings.clone();
            state.settings.custom_army = random_army_from_pool(&state.random_pool, count);
            state.random_pool_editing = false;
            previous_settings
        };

        if let Err(error) = sync_settings_with_previous(
            &document,
            Rc::clone(&state),
            SyncAction::ResetWorker,
            previous_settings,
            false,
        ) {
            log_error(&error);
        }
    });

    button.add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())?;
    closure.forget();
    Ok(())
}

fn install_random_pool_toggle_handler(
    document: &Document,
    state: Rc<RefCell<AppState>>,
) -> Result<(), JsValue> {
    let document = document.clone();
    let button = button(&document, "random-pool-toggle-button")?;
    let closure = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        {
            let mut state = state.borrow_mut();
            if state.settings.army_preset != ArmyPreset::CustomFinite {
                state.random_pool_editing = false;
            } else {
                state.random_pool_editing = !state.random_pool_editing;
            }
        }
        if let Err(error) = render_army_list(&document, Rc::clone(&state)) {
            log_error(&error);
        }
        if let Err(error) = update_outputs(&document, &state.borrow().settings) {
            log_error(&error);
        }
    });

    button.add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())?;
    closure.forget();
    Ok(())
}

fn install_button<F>(
    document: &Document,
    id: &str,
    state: Rc<RefCell<AppState>>,
    action: F,
) -> Result<(), JsValue>
where
    F: Fn(&mut AppState) -> Result<(), JsValue> + 'static,
{
    let button = document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("missing button #{id}")))?
        .dyn_into::<HtmlButtonElement>()?;
    let closure = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        if let Err(error) = action(&mut state.borrow_mut()) {
            log_error(&error);
        }
    });

    button.add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())?;
    closure.forget();
    Ok(())
}

fn install_image_export_button(
    document: &Document,
    state: Rc<RefCell<AppState>>,
    config: ImageExportButtonConfig,
) -> Result<(), JsValue> {
    let button = button(document, config.id)?;
    let button_id = config.id.to_string();
    let closure_button = button.clone();
    let closure = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        let mut state_ref = state.borrow_mut();
        if let Some(cancel) = state_ref.active_export_cancel.as_ref() {
            if state_ref.active_export_button_id.as_deref() == Some(button_id.as_str()) {
                cancel.set(true);
                closure_button.set_text_content(Some("Canceling"));
                closure_button.set_disabled(true);
                if let Err(error) = set_status("Canceling export") {
                    log_error(&error);
                }
            } else if let Err(error) = set_status("Another export is already running") {
                log_error(&error);
            }
            return;
        }

        let document = match current_document() {
            Ok(document) => document,
            Err(error) => {
                log_error(&error);
                return;
            }
        };
        if let Err(error) = sync_state_from_controls(&document, &mut state_ref, false) {
            log_error(&error);
            return;
        }

        let original_text = closure_button
            .text_content()
            .unwrap_or_else(|| "Export".to_string());
        let cancel_flag = Rc::new(Cell::new(false));
        state_ref.active_export_cancel = Some(Rc::clone(&cancel_flag));
        state_ref.active_export_button_id = Some(button_id.clone());
        state_ref.active_export_button_text = Some(original_text.clone());
        closure_button.set_text_content(Some("Cancel"));
        closure_button.set_disabled(false);
        if let Err(error) = closure_button.class_list().add_1("export-cancel-active") {
            log_error(&error);
        }

        let download_settings = render_settings_for_download(&state_ref);
        let filename = download_filename(
            &download_settings,
            state_ref.last_stats,
            config.artifact,
            config.extension,
        );
        let finish_state = Rc::clone(&state);
        let finish_button = closure_button.clone();
        let finish_button_id = button_id.clone();
        let finish = Rc::new(move |result: Result<(), String>| {
            let mut state = finish_state.borrow_mut();
            let original = state
                .active_export_button_text
                .clone()
                .unwrap_or_else(|| "Export".to_string());
            if state.active_export_button_id.as_deref() == Some(finish_button_id.as_str()) {
                state.active_export_cancel = None;
                state.active_export_button_id = None;
                state.active_export_button_text = None;
            }
            finish_button.set_text_content(Some(&original));
            finish_button.set_disabled(false);
            if let Err(error) = finish_button.class_list().remove_1("export-cancel-active") {
                log_error(&error);
            }
            drop(state);
            let status = match result {
                Ok(()) => "Export ready".to_string(),
                Err(message) if message == "Export canceled" => message,
                Err(message) => format!("Export failed: {message}"),
            };
            if let Err(error) = set_status(&status) {
                log_error(&error);
            }
        });

        match state_ref.renderer.download_image(
            config.mime_type,
            &filename,
            config.kind,
            config.encoder_quality,
            cancel_flag,
            finish,
        ) {
            Ok(()) => {
                if let Err(error) = set_status("Preparing export | Click Cancel to stop") {
                    log_error(&error);
                }
            }
            Err(error) => {
                state_ref.active_export_cancel = None;
                state_ref.active_export_button_id = None;
                state_ref.active_export_button_text = None;
                closure_button.set_text_content(Some(&original_text));
                closure_button.set_disabled(false);
                if let Err(class_error) =
                    closure_button.class_list().remove_1("export-cancel-active")
                {
                    log_error(&class_error);
                }
                if let Err(status_error) =
                    set_status(&format!("Export failed: {}", js_value_text(&error)))
                {
                    log_error(&status_error);
                }
            }
        }
    });

    button.add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())?;
    closure.forget();
    Ok(())
}

fn install_refresh_handler(
    document: &Document,
    state: Rc<RefCell<AppState>>,
) -> Result<(), JsValue> {
    let document = document.clone();
    let button = document
        .get_element_by_id("refresh-button")
        .ok_or_else(|| JsValue::from_str("missing button #refresh-button"))?
        .dyn_into::<HtmlButtonElement>()?;
    let closure = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        let settings = {
            let mut state = state.borrow_mut();
            if let Err(error) = sync_state_from_controls(&document, &mut state, true) {
                log_error(&error);
                return;
            }
            state.settings.clone()
        };
        if let Err(error) = recreate_worker_with_settings(
            &document,
            Rc::clone(&state),
            settings,
            false,
            true,
            "Refreshed | Paused",
        ) {
            log_error(&error);
        }
    });

    button.add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())?;
    closure.forget();
    Ok(())
}

fn ensure_worker_initialized(state: &mut AppState) -> Result<bool, JsValue> {
    if state.worker_initialized {
        return Ok(true);
    }
    if !state.worker_ready {
        return Ok(false);
    }

    send_to_worker(
        &state.worker,
        &AppToWorker::Initialize {
            epoch: state.worker_epoch,
            settings: state.worker_settings.clone(),
        },
    )?;
    state.worker_initialized = true;
    Ok(true)
}

fn recreate_worker_with_settings(
    document: &Document,
    state: Rc<RefCell<AppState>>,
    settings: EngineSettings,
    restart_after_ready: bool,
    clear_visible: bool,
    status: &str,
) -> Result<(), JsValue> {
    let replacement = Worker::new(&worker_script_url(document)?)?;
    let generation = {
        let mut state = state.borrow_mut();
        state.worker.set_onmessage(None);
        state.worker.terminate();
        state.worker_generation = state.worker_generation.wrapping_add(1);
        state.worker_epoch = state.worker_epoch.wrapping_add(1);
        state.worker = replacement.clone();
        state.worker_generation
    };
    install_worker_handler_on(&replacement, Rc::clone(&state), generation)?;
    schedule_worker_bootstrap_fallback(Rc::clone(&state), generation)?;

    {
        let mut state = state.borrow_mut();
        state.worker_ready = false;
        state.worker_initialized = false;
        state.start_after_worker_ready = false;
        state.step_after_worker_ready = None;
        state.allow_default_auto_start = false;
        state.radius_commit_pending = false;
        state.radius_commit_generation = state.radius_commit_generation.wrapping_add(1);
        state.run_tick_generation = state.run_tick_generation.wrapping_add(1);
        state.running = restart_after_ready;
        state.has_run = restart_after_ready || state.has_run;
        state.last_ui_refresh_ms = 0.0;
        state.render_scheduled = false;
        state.settings = settings.clone();
        state.worker_settings = settings;
        update_run_buttons(document, state.running)?;
        if clear_visible {
            state.last_stats = EngineStats::default();
            state.visible_settings = state.settings.clone();
            state.snapshot_stale = false;
            state.generation_staged = false;
            state.preserve_next_empty_worker_reset = false;
            let renderer_settings = state.settings.clone();
            state.renderer.set_settings(renderer_settings)?;
            state.renderer.set_color_saturation(1.0)?;
            state.renderer.set_generation_border_visible(true)?;
            state.renderer.clear_placements()?;
            reset_placement_log(&mut state)?;
        } else {
            state.generation_staged = true;
            state.preserve_next_empty_worker_reset = true;
            update_renderer_snapshot(&mut state)?;
        }
    }

    if restart_after_ready {
        let mut state = state.borrow_mut();
        state.start_after_worker_ready = true;
    }
    set_status(status)?;
    Ok(())
}

fn schedule_worker_bootstrap_fallback(
    state: Rc<RefCell<AppState>>,
    generation: u64,
) -> Result<(), JsValue> {
    let closure = Closure::<dyn FnMut()>::new(move || {
        let (worker, epoch, settings) = {
            let state = state.borrow();
            if state.worker_generation != generation {
                return;
            }
            if state.worker_initialized {
                return;
            }

            (
                state.worker.clone(),
                state.worker_epoch,
                state.worker_settings.clone(),
            )
        };

        if let Err(error) = send_to_worker(&worker, &AppToWorker::Initialize { epoch, settings }) {
            log_error(&error);
        }
    });

    web_sys::window()
        .ok_or_else(|| JsValue::from_str("window unavailable"))?
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            50,
        )?;
    closure.forget();
    Ok(())
}

fn mark_worker_ready_from_current_response(
    state: &mut AppState,
) -> Option<(Worker, u64, bool, Option<u32>)> {
    if state.worker_ready && state.worker_initialized {
        return None;
    }

    state.worker_ready = true;
    state.worker_initialized = true;
    let start_after_ready = state.start_after_worker_ready;
    let step_after_ready = state.step_after_worker_ready.take();
    state.start_after_worker_ready = false;

    Some((
        state.worker.clone(),
        state.worker_epoch,
        start_after_ready,
        step_after_ready,
    ))
}

fn dispatch_deferred_worker_actions(
    worker: Worker,
    epoch: u64,
    start_after_ready: bool,
    step_after_ready: Option<u32>,
) -> Result<(), JsValue> {
    if start_after_ready {
        send_to_worker(&worker, &AppToWorker::Start { epoch })?;
        send_to_worker(&worker, &AppToWorker::RunTick { epoch })?;
    } else if let Some(max_steps) = step_after_ready {
        send_to_worker(&worker, &AppToWorker::StepBatch { epoch, max_steps })?;
    }
    Ok(())
}

fn render_army_list(document: &Document, state: Rc<RefCell<AppState>>) -> Result<(), JsValue> {
    let list = document
        .get_element_by_id("army-list")
        .ok_or_else(|| JsValue::from_str("missing army-list"))?;
    list.set_inner_html("");
    let random_pool_editing = state.borrow().random_pool_editing;
    list.set_attribute(
        "data-random-pool-editing",
        if random_pool_editing { "true" } else { "false" },
    )?;
    if let Ok(toggle) = button(document, "random-pool-toggle-button") {
        toggle.set_attribute(
            "aria-pressed",
            if random_pool_editing { "true" } else { "false" },
        )?;
    }

    if random_pool_editing {
        return render_random_pool_list(document, state, &list);
    }

    let settings = state.borrow().settings.clone();
    let army = settings.custom_army.clone();
    let army_len = army.len().max(1);
    if army.is_empty() {
        let row = document.create_element("div")?;
        row.set_class_name("army-empty-row");
        list.append_child(&row)?;
        return Ok(());
    }

    for (index, piece) in army.iter().enumerate() {
        let row = document.create_element("div")?;
        row.set_class_name("army-row");
        row.set_attribute("draggable", "true")?;
        install_piece_drag_handlers(&row, document.clone(), Rc::clone(&state), index)?;

        let label = document.create_element("span")?;
        label.set_text_content(Some(&format!("{}. ({}, {})", index + 1, piece.a, piece.b)));
        row.append_child(&label)?;

        let swatch = document.create_element("span")?.dyn_into::<HtmlElement>()?;
        swatch.set_class_name("army-swatch");
        let color_t = if army_len <= 1 {
            0.0
        } else {
            index as f64 / (army_len - 1) as f64
        };
        swatch.style().set_property(
            "background",
            &rainbow_color(&settings.anchor_color_a, &settings.anchor_color_b, color_t),
        )?;
        swatch.set_title(&format!("Order color {}", index + 1));
        row.append_child(&swatch)?;

        let up = small_button(document, "▲", "Move up")?;
        install_piece_action(&up, document.clone(), Rc::clone(&state), move |army| {
            if index > 0 {
                army.swap(index, index - 1);
            }
        })?;
        row.append_child(&up)?;

        let down = small_button(document, "▼", "Move down")?;
        install_piece_action(&down, document.clone(), Rc::clone(&state), move |army| {
            if index + 1 < army.len() {
                army.swap(index, index + 1);
            }
        })?;
        row.append_child(&down)?;

        let remove = small_button(document, "Del", "Delete")?;
        install_piece_action(&remove, document.clone(), Rc::clone(&state), move |army| {
            if index < army.len() {
                army.remove(index);
            }
        })?;
        row.append_child(&remove)?;

        list.append_child(&row)?;
    }

    Ok(())
}

fn render_random_pool_list(
    document: &Document,
    state: Rc<RefCell<AppState>>,
    list: &Element,
) -> Result<(), JsValue> {
    let pool = state.borrow().random_pool.clone();
    if pool.is_empty() {
        let row = document.create_element("div")?;
        row.set_class_name("army-empty-row");
        row.set_text_content(Some("Random pool is empty."));
        list.append_child(&row)?;
        return Ok(());
    }

    for (index, piece) in pool.iter().enumerate() {
        let row = document.create_element("div")?;
        row.set_class_name("army-row random-pool-row");
        row.set_attribute("draggable", "true")?;
        install_random_pool_drag_handlers(&row, document.clone(), Rc::clone(&state), index)?;

        let label = document.create_element("span")?;
        let name = document.create_element("span")?;
        name.set_class_name("pool-piece-name");
        name.set_text_content(Some(&piece.name));
        let mov = document.create_element("span")?;
        mov.set_class_name("pool-piece-move");
        mov.set_text_content(Some(&format!("({}, {})", piece.a, piece.b)));
        label.append_child(&name)?;
        label.append_child(&mov)?;
        row.append_child(&label)?;

        let up = small_button(document, "▲", "Move up")?;
        install_random_pool_action(&up, document.clone(), Rc::clone(&state), move |pool| {
            if index > 0 {
                pool.swap(index, index - 1);
            }
        })?;
        row.append_child(&up)?;

        let down = small_button(document, "▼", "Move down")?;
        install_random_pool_action(&down, document.clone(), Rc::clone(&state), move |pool| {
            if index + 1 < pool.len() {
                pool.swap(index, index + 1);
            }
        })?;
        row.append_child(&down)?;

        let remove = small_button(document, "Del", "Delete")?;
        install_random_pool_action(&remove, document.clone(), Rc::clone(&state), move |pool| {
            if index < pool.len() {
                pool.remove(index);
            }
        })?;
        row.append_child(&remove)?;

        list.append_child(&row)?;
    }

    Ok(())
}

fn install_piece_drag_handlers(
    row: &Element,
    document: Document,
    state: Rc<RefCell<AppState>>,
    index: usize,
) -> Result<(), JsValue> {
    let drag_state = Rc::clone(&state);
    let drag_row = row.clone();
    let drag_start = Closure::<dyn FnMut(DragEvent)>::new(move |event: DragEvent| {
        drag_state.borrow_mut().dragging_army_index = Some(index);
        if let Err(error) = drag_row.class_list().add_1("drag-source") {
            log_error(&error);
        }
        if let Some(data_transfer) = event.data_transfer() {
            data_transfer.set_effect_allowed("move");
            let _ = data_transfer.set_data("text/plain", &index.to_string());
        }
    });
    row.add_event_listener_with_callback("dragstart", drag_start.as_ref().unchecked_ref())?;
    drag_start.forget();

    let over_row = row.clone();
    let drag_over = Closure::<dyn FnMut(DragEvent)>::new(move |event: DragEvent| {
        event.prevent_default();
        if let Err(error) = over_row.class_list().add_1("drag-over") {
            log_error(&error);
        }
        if let Some(data_transfer) = event.data_transfer() {
            data_transfer.set_drop_effect("move");
        }
    });
    row.add_event_listener_with_callback("dragover", drag_over.as_ref().unchecked_ref())?;
    drag_over.forget();

    let leave_row = row.clone();
    let drag_leave = Closure::<dyn FnMut(DragEvent)>::new(move |_event: DragEvent| {
        if let Err(error) = leave_row.class_list().remove_1("drag-over") {
            log_error(&error);
        }
    });
    row.add_event_listener_with_callback("dragleave", drag_leave.as_ref().unchecked_ref())?;
    drag_leave.forget();

    let drop_document = document.clone();
    let drop_state = Rc::clone(&state);
    let drop_row = row.clone();
    let drop = Closure::<dyn FnMut(DragEvent)>::new(move |event: DragEvent| {
        event.prevent_default();
        if let Err(error) = drop_row.class_list().remove_1("drag-over") {
            log_error(&error);
        }
        let (moved, previous_settings) = {
            let mut state = drop_state.borrow_mut();
            let Some(from) = state.dragging_army_index.take() else {
                return;
            };
            let previous_settings = state.settings.clone();
            let before = state.settings.custom_army.clone();
            let changed = move_custom_piece(&mut state.settings.custom_army, from, index);
            (
                changed && before != state.settings.custom_army,
                previous_settings,
            )
        };

        if moved
            && let Err(error) = sync_settings_with_previous(
                &drop_document,
                Rc::clone(&drop_state),
                SyncAction::ResetWorker,
                previous_settings,
                false,
            )
        {
            log_error(&error);
        }
    });
    row.add_event_listener_with_callback("drop", drop.as_ref().unchecked_ref())?;
    drop.forget();

    let end_state = state;
    let end_row = row.clone();
    let drag_end = Closure::<dyn FnMut(DragEvent)>::new(move |_event: DragEvent| {
        end_state.borrow_mut().dragging_army_index = None;
        if let Err(error) = end_row.class_list().remove_1("drag-source") {
            log_error(&error);
        }
        if let Err(error) = end_row.class_list().remove_1("drag-over") {
            log_error(&error);
        }
    });
    row.add_event_listener_with_callback("dragend", drag_end.as_ref().unchecked_ref())?;
    drag_end.forget();

    Ok(())
}

fn install_random_pool_drag_handlers(
    row: &Element,
    document: Document,
    state: Rc<RefCell<AppState>>,
    index: usize,
) -> Result<(), JsValue> {
    let drag_state = Rc::clone(&state);
    let drag_row = row.clone();
    let drag_start = Closure::<dyn FnMut(DragEvent)>::new(move |event: DragEvent| {
        drag_state.borrow_mut().dragging_army_index = Some(index);
        if let Err(error) = drag_row.class_list().add_1("drag-source") {
            log_error(&error);
        }
        if let Some(data_transfer) = event.data_transfer() {
            data_transfer.set_effect_allowed("move");
            let _ = data_transfer.set_data("text/plain", &index.to_string());
        }
    });
    row.add_event_listener_with_callback("dragstart", drag_start.as_ref().unchecked_ref())?;
    drag_start.forget();

    let over_row = row.clone();
    let drag_over = Closure::<dyn FnMut(DragEvent)>::new(move |event: DragEvent| {
        event.prevent_default();
        if let Err(error) = over_row.class_list().add_1("drag-over") {
            log_error(&error);
        }
        if let Some(data_transfer) = event.data_transfer() {
            data_transfer.set_drop_effect("move");
        }
    });
    row.add_event_listener_with_callback("dragover", drag_over.as_ref().unchecked_ref())?;
    drag_over.forget();

    let leave_row = row.clone();
    let drag_leave = Closure::<dyn FnMut(DragEvent)>::new(move |_event: DragEvent| {
        if let Err(error) = leave_row.class_list().remove_1("drag-over") {
            log_error(&error);
        }
    });
    row.add_event_listener_with_callback("dragleave", drag_leave.as_ref().unchecked_ref())?;
    drag_leave.forget();

    let drop_document = document.clone();
    let drop_state = Rc::clone(&state);
    let drop_row = row.clone();
    let drop = Closure::<dyn FnMut(DragEvent)>::new(move |event: DragEvent| {
        event.prevent_default();
        if let Err(error) = drop_row.class_list().remove_1("drag-over") {
            log_error(&error);
        }
        let changed = {
            let mut state = drop_state.borrow_mut();
            let Some(from) = state.dragging_army_index.take() else {
                return;
            };
            move_random_pool_piece(&mut state.random_pool, from, index)
        };
        if changed && let Err(error) = render_army_list(&drop_document, Rc::clone(&drop_state)) {
            log_error(&error);
        }
    });
    row.add_event_listener_with_callback("drop", drop.as_ref().unchecked_ref())?;
    drop.forget();

    let end_state = state;
    let end_row = row.clone();
    let drag_end = Closure::<dyn FnMut(DragEvent)>::new(move |_event: DragEvent| {
        end_state.borrow_mut().dragging_army_index = None;
        if let Err(error) = end_row.class_list().remove_1("drag-source") {
            log_error(&error);
        }
        if let Err(error) = end_row.class_list().remove_1("drag-over") {
            log_error(&error);
        }
    });
    row.add_event_listener_with_callback("dragend", drag_end.as_ref().unchecked_ref())?;
    drag_end.forget();

    Ok(())
}

fn move_custom_piece(army: &mut Vec<CustomPiece>, from: usize, to: usize) -> bool {
    if from == to || from >= army.len() || to >= army.len() {
        return false;
    }

    let piece = army.remove(from);
    let adjusted_to = if from < to { to } else { to.min(army.len()) };
    army.insert(adjusted_to, piece);
    true
}

fn move_random_pool_piece(pool: &mut Vec<RandomPoolPiece>, from: usize, to: usize) -> bool {
    if from == to || from >= pool.len() || to >= pool.len() {
        return false;
    }

    let piece = pool.remove(from);
    let adjusted_to = if from < to { to } else { to.min(pool.len()) };
    pool.insert(adjusted_to, piece);
    true
}

fn install_piece_action<F>(
    button: &HtmlButtonElement,
    document: Document,
    state: Rc<RefCell<AppState>>,
    action: F,
) -> Result<(), JsValue>
where
    F: Fn(&mut Vec<CustomPiece>) + 'static,
{
    let closure = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        let previous_settings = {
            let mut state = state.borrow_mut();
            let previous_settings = state.settings.clone();
            let before = state.settings.custom_army.clone();
            action(&mut state.settings.custom_army);
            if before == state.settings.custom_army {
                return;
            }
            previous_settings
        };

        if let Err(error) = sync_settings_with_previous(
            &document,
            Rc::clone(&state),
            SyncAction::ResetWorker,
            previous_settings,
            false,
        ) {
            log_error(&error);
        }
    });

    button.add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())?;
    closure.forget();
    Ok(())
}

fn install_random_pool_action<F>(
    button: &HtmlButtonElement,
    document: Document,
    state: Rc<RefCell<AppState>>,
    action: F,
) -> Result<(), JsValue>
where
    F: Fn(&mut Vec<RandomPoolPiece>) + 'static,
{
    let closure = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        let changed = {
            let mut state = state.borrow_mut();
            let before = state.random_pool.clone();
            action(&mut state.random_pool);
            before != state.random_pool
        };

        if changed && let Err(error) = render_army_list(&document, Rc::clone(&state)) {
            log_error(&error);
        }
    });

    button.add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())?;
    closure.forget();
    Ok(())
}

fn small_button(
    document: &Document,
    text: &str,
    title: &str,
) -> Result<HtmlButtonElement, JsValue> {
    let button = document
        .create_element("button")?
        .dyn_into::<HtmlButtonElement>()?;
    button.set_type("button");
    button.set_text_content(Some(text));
    button.set_title(title);
    Ok(button)
}

fn sync_settings(
    document: &Document,
    state: Rc<RefCell<AppState>>,
    action: SyncAction,
    commit_prime_divisor: bool,
) -> Result<(), JsValue> {
    let (previous_settings, settings) = {
        let mut state = state.borrow_mut();
        sync_state_from_controls(document, &mut state, commit_prime_divisor)?
    };
    apply_synced_settings(document, state, action, previous_settings, settings)
}

fn sync_settings_with_previous(
    document: &Document,
    state: Rc<RefCell<AppState>>,
    action: SyncAction,
    previous_settings: EngineSettings,
    commit_prime_divisor: bool,
) -> Result<(), JsValue> {
    let (previous_settings, settings) = {
        let mut state = state.borrow_mut();
        sync_state_from_controls_with_previous(
            document,
            &mut state,
            previous_settings,
            commit_prime_divisor,
        )?
    };
    apply_synced_settings(document, state, action, previous_settings, settings)
}

fn apply_synced_settings(
    document: &Document,
    state: Rc<RefCell<AppState>>,
    action: SyncAction,
    previous_settings: EngineSettings,
    settings: EngineSettings,
) -> Result<(), JsValue> {
    let action = resolve_sync_action(action, &previous_settings, &settings);

    if settings.army_preset != ArmyPreset::CustomFinite {
        state.borrow_mut().random_pool_editing = false;
    }
    render_army_list(document, Rc::clone(&state))?;

    match action {
        SyncAction::RenderOnly => {}
        SyncAction::AutoControl(id) if id == "visual-progress-toggle" => {
            let turning_visual_back_on =
                !previous_settings.visual_progress && settings.visual_progress;
            let was_running = state.borrow().running;
            if turning_visual_back_on && was_running {
                recreate_worker_with_settings(
                    document,
                    Rc::clone(&state),
                    settings,
                    false,
                    false,
                    "Visual Progress restored | Paused; restart to run visually",
                )?;
            } else {
                let mut state = state.borrow_mut();
                let worker_settings = worker_visible_settings(&state);
                state.worker_settings = worker_settings;
                if ensure_worker_initialized(&mut state)? {
                    send_to_worker(
                        &state.worker,
                        &AppToWorker::UpdateSettings {
                            epoch: state.worker_epoch,
                            settings: state.worker_settings.clone(),
                        },
                    )?;
                }
                if state.running && !state.settings.visual_progress {
                    set_status(&running_status_text(&state.settings))?;
                }
            }
        }
        SyncAction::AutoControl(_) => {}
        SyncAction::DebounceRadius => {
            schedule_radius_commit(document.clone(), Rc::clone(&state))?;
        }
        SyncAction::UpdateWorker => {
            let mut state = state.borrow_mut();
            if previous_settings.speed != settings.speed {
                state.run_tick_generation = state.run_tick_generation.wrapping_add(1);
            }
            let worker_settings = worker_visible_settings(&state);
            state.worker_settings = worker_settings;
            if ensure_worker_initialized(&mut state)? {
                send_to_worker(
                    &state.worker,
                    &AppToWorker::UpdateSettings {
                        epoch: state.worker_epoch,
                        settings: state.worker_settings.clone(),
                    },
                )?;
            }
        }
        SyncAction::ResetWorker => {
            let clear_visible = previous_settings == settings;
            recreate_worker_with_settings(
                document,
                Rc::clone(&state),
                settings,
                false,
                clear_visible,
                "Reset",
            )?;
        }
    }

    schedule_render(Rc::clone(&state))?;
    Ok(())
}

fn sync_state_from_controls(
    document: &Document,
    state: &mut AppState,
    commit_prime_divisor: bool,
) -> Result<(EngineSettings, EngineSettings), JsValue> {
    let previous_settings = state.settings.clone();
    let custom_army = previous_settings.custom_army.clone();
    apply_board_defaults(document, state, &previous_settings)?;
    let settings = read_settings(
        document,
        custom_army,
        &previous_settings,
        commit_prime_divisor,
    )?;
    update_outputs(document, &settings)?;
    state.settings = settings.clone();
    if !canvas_pan_enabled(&settings) {
        state.canvas_dragging = false;
        if let Some(canvas) = document.get_element_by_id("sim-canvas") {
            canvas.class_list().remove_1("dragging")?;
        }
    }
    remember_shape_preference(state, settings.board, settings.shape);
    update_renderer_snapshot(state)?;
    Ok((previous_settings, settings))
}

fn sync_state_from_controls_with_previous(
    document: &Document,
    state: &mut AppState,
    previous_settings: EngineSettings,
    commit_prime_divisor: bool,
) -> Result<(EngineSettings, EngineSettings), JsValue> {
    let custom_army = state.settings.custom_army.clone();
    apply_board_defaults(document, state, &previous_settings)?;
    let settings = read_settings(
        document,
        custom_army,
        &previous_settings,
        commit_prime_divisor,
    )?;
    update_outputs(document, &settings)?;
    state.settings = settings.clone();
    if !canvas_pan_enabled(&settings) {
        state.canvas_dragging = false;
        if let Some(canvas) = document.get_element_by_id("sim-canvas") {
            canvas.class_list().remove_1("dragging")?;
        }
    }
    remember_shape_preference(state, settings.board, settings.shape);
    update_renderer_snapshot(state)?;
    Ok((previous_settings, settings))
}

fn render_settings_for_download(state: &AppState) -> EngineSettings {
    let (_, placement_settings) = render_settings_for_snapshot(state);
    placement_settings
}

fn worker_visible_settings(state: &AppState) -> EngineSettings {
    let mut settings = state.settings.clone();
    if state.radius_commit_pending {
        settings.radius = state.worker_settings.radius;
    }
    settings
}

fn schedule_radius_commit(document: Document, state: Rc<RefCell<AppState>>) -> Result<(), JsValue> {
    let generation = {
        let mut state = state.borrow_mut();
        state.radius_commit_pending = true;
        state.radius_commit_generation = state.radius_commit_generation.wrapping_add(1);
        state.radius_commit_generation
    };

    let closure = Closure::<dyn FnMut()>::new(move || {
        let should_commit = {
            let state = state.borrow();
            state.radius_commit_pending && state.radius_commit_generation == generation
        };

        if should_commit {
            let mut state = state.borrow_mut();
            if let Err(error) = commit_pending_radius_change(&mut state, &document, false, false) {
                log_error(&error);
            }
        }
    });

    web_sys::window()
        .ok_or_else(|| JsValue::from_str("window unavailable"))?
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            RADIUS_COMMIT_DELAY_MS,
        )?;
    closure.forget();
    Ok(())
}

fn commit_pending_radius_change(
    state: &mut AppState,
    document: &Document,
    restart_after_reset: bool,
    clear_visible: bool,
) -> Result<(), JsValue> {
    if !state.radius_commit_pending {
        return Ok(());
    }

    let settings = state.settings.clone();
    if radius_increase_can_update_worker(&state.worker_settings, &settings) {
        state.radius_commit_pending = false;
        state.radius_commit_generation = state.radius_commit_generation.wrapping_add(1);
        state.worker_settings = settings.clone();
        state.generation_staged = false;
        state.preserve_next_empty_worker_reset = false;
        update_renderer_snapshot(state)?;

        if state.worker_ready {
            if ensure_worker_initialized(state)? {
                send_to_worker(
                    &state.worker,
                    &AppToWorker::UpdateSettings {
                        epoch: state.worker_epoch,
                        settings: settings.clone(),
                    },
                )?;
            }
            if restart_after_reset && !state.running {
                state.running = true;
                update_run_buttons(document, state.running)?;
                let epoch = state.worker_epoch;
                send_to_worker(&state.worker, &AppToWorker::Start { epoch })?;
                send_to_worker(&state.worker, &AppToWorker::RunTick { epoch })?;
            }
        }
        return Ok(());
    }

    reset_worker_with_settings(
        state,
        document,
        settings,
        restart_after_reset,
        clear_visible,
    )
}

fn radius_increase_can_update_worker(current: &EngineSettings, next: &EngineSettings) -> bool {
    next.radius > current.radius
        && current.board == next.board
        && current.placement_search == PlacementSearchMode::SpiralPath
        && next.placement_search == PlacementSearchMode::SpiralPath
        && continuous_piece_radius_matches(current, next)
        && current.proactive_attacking == next.proactive_attacking
        && current.enemy_mode == next.enemy_mode
        && current.army_preset == next.army_preset
        && current.custom_army == next.custom_army
        && current.continuous_offset == next.continuous_offset
        && current.prime_modulo_divisor == next.prime_modulo_divisor
}

fn reset_worker_with_settings(
    state: &mut AppState,
    document: &Document,
    settings: EngineSettings,
    restart_after_reset: bool,
    clear_visible: bool,
) -> Result<(), JsValue> {
    state.radius_commit_pending = false;
    state.radius_commit_generation = state.radius_commit_generation.wrapping_add(1);
    state.step_after_worker_ready = None;
    state.running = restart_after_reset;
    state.has_run = restart_after_reset || state.has_run;
    state.last_ui_refresh_ms = 0.0;
    state.render_scheduled = false;
    state.worker_epoch = state.worker_epoch.wrapping_add(1);
    let epoch = state.worker_epoch;
    state.worker_settings = settings.clone();
    let can_reset_now = state.worker_ready;
    state.worker_initialized = can_reset_now;
    update_run_buttons(document, state.running)?;
    if clear_visible {
        state.last_stats = EngineStats::default();
        state.visible_settings = settings.clone();
        state.snapshot_stale = false;
        state.generation_staged = false;
        state.preserve_next_empty_worker_reset = false;
        state.renderer.set_settings(settings.clone())?;
        state.renderer.set_color_saturation(1.0)?;
        state.renderer.set_generation_border_visible(true)?;
        state.renderer.clear_placements()?;
        reset_placement_log(state)?;
    } else {
        state.generation_staged = true;
        state.preserve_next_empty_worker_reset = true;
        update_renderer_snapshot(state)?;
    }
    if !can_reset_now {
        return Ok(());
    }

    send_to_worker(&state.worker, &AppToWorker::Reset { epoch, settings })?;
    if restart_after_reset {
        send_to_worker(&state.worker, &AppToWorker::Start { epoch })?;
        send_to_worker(&state.worker, &AppToWorker::RunTick { epoch })?;
    }
    Ok(())
}

fn apply_board_defaults(
    document: &Document,
    state: &AppState,
    previous: &EngineSettings,
) -> Result<(), JsValue> {
    let next_board = parse_board_kind(&select_value(document, "board-select")?, previous.board);

    if previous.board != next_board {
        input(document, "piece-radius-slider")?.set_value("0.50");

        if next_board == BoardKind::ContinuousArchimedean {
            set_select_value(document, "shape-select", "Circle")?;
        } else {
            set_select_value(
                document,
                "shape-select",
                shape_value(preferred_shape_for_board(state, next_board)),
            )?;
        }
    }

    Ok(())
}

fn preferred_shape_for_board(state: &AppState, board: BoardKind) -> ShapeKind {
    match board {
        BoardKind::LatticeSquare => state.preferred_square_shape,
        BoardKind::LatticeHex => state.preferred_hex_shape,
        BoardKind::LatticeTriangle => state.preferred_triangle_shape,
        BoardKind::ContinuousArchimedean => ShapeKind::Circle,
    }
}

fn remember_shape_preference(state: &mut AppState, board: BoardKind, shape: ShapeKind) {
    match board {
        BoardKind::LatticeSquare => {
            if matches!(
                shape,
                ShapeKind::Square | ShapeKind::Circle | ShapeKind::Hex
            ) {
                state.preferred_square_shape = shape;
            }
        }
        BoardKind::LatticeHex => {
            if matches!(
                shape,
                ShapeKind::Square | ShapeKind::Circle | ShapeKind::Hex
            ) {
                state.preferred_hex_shape = shape;
            }
        }
        BoardKind::LatticeTriangle => {
            if matches!(shape, ShapeKind::Triangle | ShapeKind::Circle) {
                state.preferred_triangle_shape = shape;
            }
        }
        BoardKind::ContinuousArchimedean => {}
    }
}

fn shape_value(shape: ShapeKind) -> &'static str {
    match shape {
        ShapeKind::Square => "Square",
        ShapeKind::Circle => "Circle",
        ShapeKind::Hex => "Hex",
        ShapeKind::Triangle => "Triangle",
    }
}

fn resolve_sync_action(
    action: SyncAction,
    previous: &EngineSettings,
    next: &EngineSettings,
) -> SyncAction {
    match action {
        SyncAction::AutoControl(id) if id == "shape-select" => SyncAction::RenderOnly,
        SyncAction::AutoControl(id) if id == "radius-input" => SyncAction::DebounceRadius,
        SyncAction::AutoControl(id) if id == "piece-radius-slider" => {
            if previous.board == BoardKind::ContinuousArchimedean
                || next.board == BoardKind::ContinuousArchimedean
            {
                SyncAction::ResetWorker
            } else {
                SyncAction::RenderOnly
            }
        }
        other => other,
    }
}

fn read_settings(
    document: &Document,
    custom_army: Vec<CustomPiece>,
    fallback: &EngineSettings,
    commit_prime_divisor: bool,
) -> Result<EngineSettings, JsValue> {
    let board = parse_board_kind(&select_value(document, "board-select")?, fallback.board);

    let shape = match board {
        BoardKind::ContinuousArchimedean => {
            set_select_value(document, "shape-select", "Circle")?;
            ShapeKind::Circle
        }
        BoardKind::LatticeTriangle => match select_value(document, "shape-select")?.as_str() {
            "Circle" => ShapeKind::Circle,
            _ => {
                set_select_value(document, "shape-select", "Triangle")?;
                ShapeKind::Triangle
            }
        },
        BoardKind::LatticeSquare | BoardKind::LatticeHex => {
            match select_value(document, "shape-select")?.as_str() {
                "Circle" => ShapeKind::Circle,
                "Hex" => ShapeKind::Hex,
                _ => ShapeKind::Square,
            }
        }
    };

    let speed = if input_checked(document, "fastest-toggle")? {
        SpeedMode::Fastest
    } else {
        SpeedMode::PerSecond(
            input_value(document, "speed-slider")?
                .parse()
                .unwrap_or(250),
        )
    };

    let display_mode = match select_value(document, "display-mode-select")?.as_str() {
        "PixelOneToOne" => DisplayMode::PixelOneToOne,
        _ => DisplayMode::FitScreen,
    };

    let army_preset = match select_value(document, "army-preset-select")?.as_str() {
        "PrimeKnight" => ArmyPreset::PrimeKnight,
        "PrimeGapper" => ArmyPreset::PrimeGapper,
        _ => ArmyPreset::CustomFinite,
    };

    let enemy_mode = match select_value(document, "enemy-mode-select")?.as_str() {
        "Color" => EnemyMode::Color,
        "ColorAttackSet" => EnemyMode::ColorAttackSet,
        _ => EnemyMode::AttackSet,
    };

    let placement_search = match select_value(document, "placement-search-select")?.as_str() {
        "CenterDistance" => PlacementSearchMode::CenterDistance,
        _ => PlacementSearchMode::SpiralPath,
    };

    let continuous_offset = read_continuous_offset(document, fallback.continuous_offset)?;

    Ok(EngineSettings {
        board,
        shape,
        radius: parse_finite_f64(&input_value(document, "radius-input")?, DEFAULT_RADIUS).max(1.0),
        piece_radius: parse_finite_f64(&input_value(document, "piece-radius-slider")?, 0.5)
            .clamp(0.05, 0.5),
        visual_progress: input_checked(document, "visual-progress-toggle")?,
        speed,
        display_mode,
        zoom: input_value(document, "zoom-slider")?
            .parse::<u8>()
            .unwrap_or(1)
            .clamp(1, 32),
        track_opacity: parse_finite_f64(&input_value(document, "track-opacity-slider")?, 0.0)
            .clamp(0.0, 100.0) as f32
            / 100.0,
        attack_overlay_opacity: parse_finite_f64(
            &input_value(document, "attack-overlay-opacity-slider")?,
            0.0,
        )
        .clamp(0.0, 100.0) as f32
            / 100.0,
        proactive_attacking: input_checked(document, "attacking-toggle")?,
        enemy_mode,
        placement_search,
        army_preset,
        custom_army: normalize_custom_army(custom_army),
        continuous_offset,
        prime_modulo_divisor: read_prime_modulo_divisor(document, fallback, commit_prime_divisor)?,
        anchor_color_a: input_value(document, "anchor-a-input")?,
        anchor_color_b: input_value(document, "anchor-b-input")?,
    })
}

fn read_prime_modulo_divisor(
    document: &Document,
    fallback: &EngineSettings,
    commit: bool,
) -> Result<u32, JsValue> {
    let raw = input_value(document, "prime-divisor-input")?;
    let parsed = raw.trim().parse::<u32>().ok();
    let divisor = if commit {
        parsed
            .map(normalize_prime_modulo_divisor)
            .unwrap_or(fallback.prime_modulo_divisor)
    } else {
        parsed
            .filter(|value| *value >= 6 && value.is_multiple_of(6))
            .unwrap_or(fallback.prime_modulo_divisor)
    };
    if commit {
        input(document, "prime-divisor-input")?.set_value(&divisor.to_string());
    }
    Ok(divisor)
}

fn parse_board_kind(value: &str, fallback: BoardKind) -> BoardKind {
    match value {
        "LatticeSquare" => BoardKind::LatticeSquare,
        "LatticeHex" => BoardKind::LatticeHex,
        "LatticeTriangle" => BoardKind::LatticeTriangle,
        "ContinuousArchimedean" => BoardKind::ContinuousArchimedean,
        _ => fallback,
    }
}

fn parse_finite_f64(raw: &str, fallback: f64) -> f64 {
    match raw.trim().parse::<f64>() {
        Ok(value) if value.is_finite() => value,
        _ => fallback,
    }
}

fn update_outputs(document: &Document, settings: &EngineSettings) -> Result<(), JsValue> {
    set_text(
        document,
        "speed-output",
        &match settings.speed {
            SpeedMode::Fastest => "Fastest".to_string(),
            SpeedMode::PerSecond(value) => format!("{value}/s"),
        },
    )?;
    set_text(
        document,
        "piece-radius-output",
        &format!("{:.2}", settings.piece_radius),
    )?;
    let track_text = if settings.track_opacity <= f32::EPSILON {
        "Off".to_string()
    } else {
        format!("{}%", (settings.track_opacity * 100.0).round() as u32)
    };
    set_text(document, "track-opacity-output", &track_text)?;
    let attack_overlay_text = if settings.attack_overlay_opacity <= f32::EPSILON {
        "Off".to_string()
    } else {
        format!(
            "{}%",
            (settings.attack_overlay_opacity * 100.0).round() as u32
        )
    };
    set_text(
        document,
        "attack-overlay-opacity-output",
        &attack_overlay_text,
    )?;

    input(document, "speed-slider")?.set_disabled(matches!(settings.speed, SpeedMode::Fastest));
    let continuous = settings.board == BoardKind::ContinuousArchimedean;
    let triangle = settings.board == BoardKind::LatticeTriangle;
    if continuous {
        set_select_value(document, "shape-select", "Circle")?;
    } else if triangle && !matches!(settings.shape, ShapeKind::Circle | ShapeKind::Triangle) {
        set_select_value(document, "shape-select", "Triangle")?;
    }
    select(document, "shape-select")?.set_disabled(continuous);
    set_option_disabled(document, "shape-option-square", triangle)?;
    set_option_disabled(document, "shape-option-hex", triangle)?;
    set_option_disabled(document, "shape-option-triangle", !triangle)?;
    input(document, "continuous-offset-input")?.set_disabled(!continuous);
    set_disabled_class(document, "continuous-offset-group", !continuous)?;
    select(document, "enemy-mode-select")?.set_disabled(false);

    let zoom_row = html_element(document, "zoom-row")?;
    zoom_row.style().set_property("display", "none")?;
    set_text(
        document,
        "zoom-output",
        &format!("x{}", input_value(document, "zoom-slider")?),
    )?;
    update_canvas_pan_class(document, settings)?;

    let custom_display = if settings.army_preset == ArmyPreset::CustomFinite {
        "grid"
    } else {
        "none"
    };
    html_element(document, "custom-army-editor")?
        .style()
        .set_property("display", custom_display)?;

    html_element(document, "prime-color-controls")?
        .style()
        .set_property("display", "grid")?;
    html_element(document, "prime-divisor-label")?
        .style()
        .set_property(
            "display",
            if settings.army_preset == ArmyPreset::PrimeKnight {
                "grid"
            } else {
                "none"
            },
        )?;
    button(document, "add-piece-button")?
        .set_disabled(settings.army_preset != ArmyPreset::CustomFinite);
    button(document, "random-piece-button")?
        .set_disabled(settings.army_preset != ArmyPreset::CustomFinite);
    input(document, "random-count-input")?
        .set_disabled(settings.army_preset != ArmyPreset::CustomFinite);
    let random_pool_toggle = button(document, "random-pool-toggle-button")?;
    random_pool_toggle.set_disabled(settings.army_preset != ArmyPreset::CustomFinite);
    let random_pool_editing = document
        .get_element_by_id("army-list")
        .and_then(|list| list.get_attribute("data-random-pool-editing"))
        .is_some_and(|value| value == "true");
    random_pool_toggle.set_attribute(
        "aria-pressed",
        if random_pool_editing { "true" } else { "false" },
    )?;
    input(document, "prime-divisor-input")?
        .set_disabled(settings.army_preset != ArmyPreset::PrimeKnight);
    set_disabled_class(
        document,
        "prime-divisor-label",
        settings.army_preset != ArmyPreset::PrimeKnight,
    )?;
    input(document, "anchor-a-input")?.set_disabled(false);
    input(document, "anchor-b-input")?.set_disabled(false);

    Ok(())
}

fn update_canvas_pan_class(document: &Document, settings: &EngineSettings) -> Result<(), JsValue> {
    document
        .get_element_by_id("sim-canvas")
        .ok_or_else(|| JsValue::from_str("missing canvas"))?
        .class_list()
        .toggle_with_force("pan-enabled", canvas_pan_enabled(settings))?;
    Ok(())
}

fn canvas_pan_enabled(settings: &EngineSettings) -> bool {
    settings.display_mode == DisplayMode::PixelOneToOne
}

fn read_continuous_offset(document: &Document, fallback: f64) -> Result<f64, JsValue> {
    let input = input(document, "continuous-offset-input")?;
    let raw = input.value();
    let (valid, invalid_chars) = validate_continuous_offset_text(&raw);
    input
        .class_list()
        .toggle_with_force("invalid-input", !valid)?;
    html_element(document, "continuous-offset-input-wrap")?
        .class_list()
        .toggle_with_force("invalid-offset", !valid)?;
    html_element(document, "continuous-offset-highlight")?
        .set_inner_html(&continuous_offset_highlight_html(&raw, &invalid_chars));
    if valid {
        Ok(raw.parse::<f64>().unwrap_or(0.0))
    } else {
        Ok(fallback)
    }
}

fn validate_continuous_offset_text(raw: &str) -> (bool, Vec<bool>) {
    let chars = raw.chars().collect::<Vec<_>>();
    let mut invalid_chars = vec![false; chars.len()];
    if raw.is_empty() {
        return (false, invalid_chars);
    }

    let mut saw_dot = false;
    let mut saw_digit = false;
    let mut fraction_digits = 0_usize;
    let mut structurally_valid = true;
    for (index, ch) in chars.iter().enumerate() {
        if ch.is_ascii_digit() {
            saw_digit = true;
            if saw_dot {
                fraction_digits += 1;
                if fraction_digits > 12 {
                    invalid_chars[index] = true;
                    structurally_valid = false;
                }
            }
        } else if *ch == '.' && !saw_dot {
            saw_dot = true;
        } else {
            invalid_chars[index] = true;
            structurally_valid = false;
        }
    }

    if !saw_digit {
        invalid_chars.fill(true);
        structurally_valid = false;
    }

    let in_range = if structurally_valid {
        raw.parse::<f64>()
            .map(|value| (0.0..=1.0).contains(&value))
            .unwrap_or(false)
    } else {
        false
    };
    if structurally_valid && !in_range {
        invalid_chars.fill(true);
    }

    (structurally_valid && in_range, invalid_chars)
}

fn continuous_offset_highlight_html(raw: &str, invalid_chars: &[bool]) -> String {
    raw.chars()
        .enumerate()
        .map(|(index, ch)| {
            let class = if invalid_chars.get(index).copied().unwrap_or(false) {
                "invalid-char"
            } else {
                "valid-char"
            };
            format!("<span class=\"{class}\">{}</span>", html_escape_char(ch))
        })
        .collect::<Vec<_>>()
        .join("")
}

fn continuous_offset_value_text(value: f64) -> String {
    let text = format!("{:.12}", value.clamp(0.0, 1.0));
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

fn html_escape_char(ch: char) -> String {
    match ch {
        '&' => "&amp;".to_string(),
        '<' => "&lt;".to_string(),
        '>' => "&gt;".to_string(),
        '"' => "&quot;".to_string(),
        '\'' => "&#39;".to_string(),
        _ => ch.to_string(),
    }
}

fn update_run_buttons(document: &Document, running: bool) -> Result<(), JsValue> {
    button(document, "start-button")?.set_disabled(running);
    button(document, "pause-button")?.set_disabled(!running);
    button(document, "step-button")?.set_disabled(running);
    Ok(())
}

fn prepare_new_generation_if_staged(
    state: &mut AppState,
    document: &Document,
) -> Result<(), JsValue> {
    let staged_settings = state.last_stats.placements > 0
        && !simulation_settings_match(&state.visible_settings, &state.settings);
    if state.snapshot_stale
        || staged_settings
        || state.generation_staged
        || state.preserve_next_empty_worker_reset
        || !simulation_settings_match(&state.worker_settings, &state.settings)
    {
        let settings = state.settings.clone();
        reset_worker_with_settings(state, document, settings, false, true)?;
    }
    Ok(())
}

fn maybe_auto_start_default(
    document: &Document,
    state: Rc<RefCell<AppState>>,
) -> Result<(), JsValue> {
    let mut state = state.borrow_mut();
    if state.has_run
        || state.running
        || state.last_stats.placements > 0
        || !is_default_simple_settings(&state.settings)
    {
        update_run_buttons(document, state.running)?;
        return Ok(());
    }

    state.run_tick_generation = state.run_tick_generation.wrapping_add(1);
    state.running = true;
    state.has_run = true;
    update_run_buttons(document, state.running)?;
    set_status(&running_status_text(&state.settings))?;
    let epoch = state.worker_epoch;
    send_to_worker(&state.worker, &AppToWorker::Start { epoch })?;
    send_to_worker(&state.worker, &AppToWorker::RunTick { epoch })
}

fn running_status_text(settings: &EngineSettings) -> String {
    if settings.visual_progress {
        "Running".to_string()
    } else {
        "Running silently | Visual Progress is off; canvas and log update when the worker yields or completes"
            .to_string()
    }
}

fn is_default_simple_settings(settings: &EngineSettings) -> bool {
    let default = EngineSettings::default();
    settings == &default
}

fn status_text(state: &AppState, stats: EngineStats) -> String {
    let exhausted = stats.exhausted;
    let stats = stats_text(&state.settings, stats);
    if exhausted {
        stats
    } else if state.running && !state.settings.visual_progress {
        format!("Running silently | {stats}")
    } else if !state.running && state.has_run {
        format!("Paused | {stats}")
    } else {
        stats
    }
}

fn stats_text(settings: &EngineSettings, stats: EngineStats) -> String {
    let mut text = match settings.army_preset {
        ArmyPreset::CustomFinite => {
            format!(
                "{} placements | radius {:.2}/{:.2} | {} spot checks",
                stats.placements, stats.current_radius, settings.radius, stats.spots_tested
            )
        }
        ArmyPreset::PrimeKnight | ArmyPreset::PrimeGapper => format!(
            "{} placements | radius {:.2}/{:.2} | {} prime spot checks | {} skipped spots",
            stats.placements,
            stats.current_radius,
            settings.radius,
            stats.piece_candidates_tested,
            stats.skipped_spots
        ),
    };

    if settings.proactive_attacking {
        text.push_str(&format!(" | {} active rejects", stats.proactive_rejections));
    }

    if stats.exhausted {
        format!("Complete | {text}")
    } else {
        text
    }
}

fn normalize_custom_army(army: Vec<CustomPiece>) -> Vec<CustomPiece> {
    army.into_iter()
        .map(|piece| CustomPiece::with_auto_color(piece.a, piece.b))
        .collect()
}

fn default_random_pool() -> Vec<RandomPoolPiece> {
    [
        ("Knight", 2, 1),
        ("Fers", 1, 1),
        ("Vazir", 1, 0),
        ("Camel", 3, 1),
        ("Zebra", 3, 2),
        ("Antelope", 4, 3),
        ("Eland", 5, 3),
        ("Satrap", 2, 0),
        ("Aspbad", 2, 2),
        ("Spehbed", 3, 0),
        ("Marzban", 3, 3),
    ]
    .into_iter()
    .map(|(name, a, b)| RandomPoolPiece {
        name: name.to_string(),
        a,
        b,
    })
    .collect()
}

fn random_pool_piece_name(a: i32, b: i32) -> Option<&'static str> {
    [
        ("Knight", 2, 1),
        ("Fers", 1, 1),
        ("Vazir", 1, 0),
        ("Camel", 3, 1),
        ("Zebra", 3, 2),
        ("Antelope", 4, 3),
        ("Eland", 5, 3),
        ("Satrap", 2, 0),
        ("Aspbad", 2, 2),
        ("Spehbed", 3, 0),
        ("Marzban", 3, 3),
    ]
    .into_iter()
    .find_map(|(name, piece_a, piece_b)| (a == piece_a && b == piece_b).then_some(name))
}

fn random_army_from_pool(pool: &[RandomPoolPiece], count: usize) -> Vec<CustomPiece> {
    if pool.is_empty() || count == 0 {
        return Vec::new();
    }

    let mut bag = Vec::<usize>::new();
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        if bag.is_empty() {
            bag.extend(0..pool.len());
            shuffle_indices(&mut bag);
        }
        if let Some(index) = bag.pop() {
            let piece = &pool[index];
            out.push(CustomPiece::with_auto_color(piece.a, piece.b));
        }
    }
    out
}

fn shuffle_indices(indices: &mut [usize]) {
    for i in (1..indices.len()).rev() {
        let j = (js_sys::Math::random() * (i + 1) as f64).floor() as usize;
        indices.swap(i, j.min(i));
    }
}

fn send_to_worker(worker: &Worker, msg: &AppToWorker) -> Result<(), JsValue> {
    let bytes = bincode::serialize(msg)
        .map_err(|error| JsValue::from_str(&format!("failed to encode worker message: {error}")))?;
    let bytes = js_sys::Uint8Array::from(bytes.as_slice());
    let transfer = js_sys::Array::new();
    transfer.push(&bytes.buffer());
    worker.post_message_with_transfer(&bytes, &transfer)
}

fn decode_worker_message(event: MessageEvent) -> Result<WorkerToApp, String> {
    let bytes = js_sys::Uint8Array::new(&event.data());
    let mut buffer = vec![0_u8; bytes.length() as usize];
    bytes.copy_to(&mut buffer);
    bincode::deserialize::<WorkerToApp>(&buffer).map_err(|error| error.to_string())
}

fn current_document() -> Result<Document, JsValue> {
    web_sys::window()
        .ok_or_else(|| JsValue::from_str("window unavailable"))?
        .document()
        .ok_or_else(|| JsValue::from_str("document unavailable"))
}

fn worker_script_url(document: &Document) -> Result<String, JsValue> {
    let base = document
        .base_uri()?
        .ok_or_else(|| JsValue::from_str("document base URI unavailable"))?;
    let url = Url::new_with_base("spg_worker_loader.js", &base)?;
    Ok(url.href())
}

fn set_status(text: &str) -> Result<(), JsValue> {
    let document = current_document()?;
    set_text(&document, "status-line", text)
}

fn set_text(document: &Document, id: &str, text: &str) -> Result<(), JsValue> {
    document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("missing element #{id}")))?
        .set_text_content(Some(text));
    Ok(())
}

fn select_value(document: &Document, id: &str) -> Result<String, JsValue> {
    Ok(select(document, id)?.value())
}

fn select(document: &Document, id: &str) -> Result<HtmlSelectElement, JsValue> {
    Ok(document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("missing select #{id}")))?
        .dyn_into::<HtmlSelectElement>()?)
}

fn set_select_value(document: &Document, id: &str, value: &str) -> Result<(), JsValue> {
    select(document, id)?.set_value(value);
    Ok(())
}

fn set_option_disabled(document: &Document, id: &str, disabled: bool) -> Result<(), JsValue> {
    let option = element(document, id)?;
    if disabled {
        option.set_attribute("disabled", "disabled")?;
    } else {
        option.remove_attribute("disabled")?;
    }
    Ok(())
}

fn set_disabled_class(document: &Document, id: &str, disabled: bool) -> Result<(), JsValue> {
    let item = element(document, id)?;
    if disabled {
        item.class_list().add_1("is-disabled")?;
    } else {
        item.class_list().remove_1("is-disabled")?;
    }
    Ok(())
}

fn input_value(document: &Document, id: &str) -> Result<String, JsValue> {
    Ok(input(document, id)?.value())
}

fn input_checked(document: &Document, id: &str) -> Result<bool, JsValue> {
    Ok(input(document, id)?.checked())
}

fn input(document: &Document, id: &str) -> Result<HtmlInputElement, JsValue> {
    Ok(document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("missing input #{id}")))?
        .dyn_into::<HtmlInputElement>()?)
}

fn button(document: &Document, id: &str) -> Result<HtmlButtonElement, JsValue> {
    Ok(document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("missing button #{id}")))?
        .dyn_into::<HtmlButtonElement>()?)
}

fn html_element(document: &Document, id: &str) -> Result<HtmlElement, JsValue> {
    Ok(document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("missing element #{id}")))?
        .dyn_into::<HtmlElement>()?)
}

fn element(document: &Document, id: &str) -> Result<Element, JsValue> {
    document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("missing element #{id}")))
}

fn log_error(error: &JsValue) {
    web_sys::console::error_1(error);
}

fn js_value_text(value: &JsValue) -> String {
    value.as_string().unwrap_or_else(|| format!("{value:?}"))
}
