use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{
    Blob, BlobPropertyBag, Document, DragEvent, Element, Event, HtmlAnchorElement,
    HtmlButtonElement, HtmlElement, HtmlInputElement, HtmlSelectElement, MessageEvent, Url, Worker,
};

use crate::protocol::{
    AppToWorker, ArmyPreset, BoardKind, CustomPiece, DisplayMode, EnemyMode, EngineSettings,
    EngineStats, Placement, ShapeKind, SpeedMode, SpotCoord, VertexBufferUpdate, WorkerToApp,
    piece_gap, rainbow_color,
};
use crate::render::CanvasRenderer;

const RADIUS_COMMIT_DELAY_MS: i32 = 2_000;

struct AppState {
    worker: Worker,
    renderer: CanvasRenderer,
    settings: EngineSettings,
    worker_settings: EngineSettings,
    last_stats: EngineStats,
    running: bool,
    has_run: bool,
    dragging_army_index: Option<usize>,
    first_log_lines: Vec<String>,
    recent_log_lines: Vec<String>,
    total_logged: u64,
    last_ui_refresh_ms: f64,
    render_scheduled: bool,
    radius_commit_pending: bool,
    radius_commit_generation: u64,
}

#[derive(Clone)]
enum SyncAction {
    RenderOnly,
    UpdateWorker,
    ResetWorker,
    DebounceRadius,
    AutoControl(String),
}

pub fn boot_app() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();

    let document = current_document()?;
    let worker = Worker::new("./spg_worker_loader.js")?;
    let renderer = CanvasRenderer::new("sim-canvas")?;
    let settings = read_settings(&document, EngineSettings::default().custom_army)?;

    let state = Rc::new(RefCell::new(AppState {
        worker,
        renderer,
        worker_settings: settings.clone(),
        settings,
        last_stats: EngineStats::default(),
        running: false,
        has_run: false,
        dragging_army_index: None,
        first_log_lines: Vec::new(),
        recent_log_lines: Vec::new(),
        total_logged: 0,
        last_ui_refresh_ms: 0.0,
        render_scheduled: false,
        radius_commit_pending: false,
        radius_commit_generation: 0,
    }));

    install_worker_handler(Rc::clone(&state))?;
    install_resize_handler(Rc::clone(&state))?;
    install_control_handlers(&document, Rc::clone(&state))?;
    render_army_list(&document, Rc::clone(&state))?;
    update_outputs(&document, &state.borrow().settings)?;
    update_run_buttons(&document, state.borrow().running)?;
    let settings = state.borrow().settings.clone();
    state.borrow_mut().renderer.set_settings(settings)?;
    set_status("Loading worker")?;

    Ok(())
}

fn install_worker_handler(state: Rc<RefCell<AppState>>) -> Result<(), JsValue> {
    let callback_state = Rc::clone(&state);
    let closure = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        let msg = decode_worker_message(event);
        match msg {
            Ok(WorkerToApp::Ready) => {
                let state = callback_state.borrow();
                if let Err(error) = send_to_worker(
                    &state.worker,
                    &AppToWorker::Initialize {
                        settings: state.worker_settings.clone(),
                    },
                ) {
                    log_error(&error);
                }
                if let Err(error) = set_status("Worker ready") {
                    log_error(&error);
                }
                if let Ok(document) = current_document() {
                    if let Err(error) = update_run_buttons(&document, state.running) {
                        log_error(&error);
                    }
                }
            }
            Ok(WorkerToApp::Batch {
                log_placements,
                vertex_update,
                stats,
                color_state,
            }) => {
                let exhausted = stats.exhausted;
                let status = {
                    let mut state = callback_state.borrow_mut();
                    if let Err(error) = state.renderer.apply_batch(&vertex_update, color_state) {
                        log_error(&error);
                    }
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
                    if should_refresh_worker_ui(&mut state, exhausted) {
                        if let Err(error) = refresh_placement_log(&state) {
                            log_error(&error);
                        }
                        Some(status_text(&state, stats))
                    } else {
                        None
                    }
                };
                if let Some(status) = status {
                    if let Err(error) = set_status(&status) {
                        log_error(&error);
                    }
                }
                if let Err(error) = schedule_render(Rc::clone(&callback_state)) {
                    log_error(&error);
                }
                if exhausted {
                    if let Ok(document) = current_document() {
                        if let Err(error) =
                            update_run_buttons(&document, callback_state.borrow().running)
                        {
                            log_error(&error);
                        }
                    }
                } else if callback_state.borrow().running {
                    if let Err(error) = schedule_next_run_tick(Rc::clone(&callback_state)) {
                        log_error(&error);
                    }
                }
            }
            Ok(WorkerToApp::Stats {
                stats,
                color_state,
                vertex_update,
            }) => {
                let exhausted = stats.exhausted;
                let needs_render = !matches!(vertex_update, VertexBufferUpdate::None);
                let status = {
                    let mut state = callback_state.borrow_mut();
                    if let Err(error) = state.renderer.apply_stats(&vertex_update, color_state) {
                        log_error(&error);
                    }
                    state.last_stats = stats;
                    if exhausted {
                        state.running = false;
                        state.has_run = true;
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
                if needs_render {
                    if let Err(error) = schedule_render(Rc::clone(&callback_state)) {
                        log_error(&error);
                    }
                }
                if exhausted {
                    if let Ok(document) = current_document() {
                        if let Err(error) =
                            update_run_buttons(&document, callback_state.borrow().running)
                        {
                            log_error(&error);
                        }
                    }
                } else if callback_state.borrow().running {
                    if let Err(error) = schedule_next_run_tick(Rc::clone(&callback_state)) {
                        log_error(&error);
                    }
                }
            }
            Ok(WorkerToApp::Error { message }) => {
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

    state
        .borrow()
        .worker
        .set_onmessage(Some(closure.as_ref().unchecked_ref()));
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
    state.first_log_lines.clear();
    state.recent_log_lines.clear();
    state.total_logged = 0;
    refresh_placement_log(state)
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
        "settings: board={:?} army={:?} enemy={:?} attacking={} radius={:.2} piece_radius={:.2} offset={:.3} anchors={}..{}\nplacements logged: {}\n\nfirst placements:\n",
        state.settings.board,
        state.settings.army_preset,
        state.settings.enemy_mode,
        state.settings.proactive_attacking,
        state.settings.radius,
        state.settings.piece_radius,
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
    let delay_ms = run_delay_ms(&state.borrow().settings.speed);
    let closure = Closure::<dyn FnMut()>::new(move || {
        let state = state.borrow();
        if state.running {
            if let Err(error) = send_to_worker(&state.worker, &AppToWorker::RunTick) {
                log_error(&error);
            }
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

fn run_delay_ms(speed: &SpeedMode) -> i32 {
    match speed {
        SpeedMode::Fastest => 0,
        SpeedMode::PerSecond(_) => 50,
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

fn install_control_handlers(
    document: &Document,
    state: Rc<RefCell<AppState>>,
) -> Result<(), JsValue> {
    for id in [
        "board-select",
        "continuous-offset-input",
        "attacking-toggle",
        "enemy-mode-select",
        "army-preset-select",
        "prime-divisor-input",
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

    for id in ["display-mode-select", "zoom-slider"] {
        install_settings_handler(document, id, Rc::clone(&state), SyncAction::RenderOnly)?;
    }

    for id in ["anchor-a-input", "anchor-b-input"] {
        install_settings_handler(document, id, Rc::clone(&state), SyncAction::UpdateWorker)?;
    }

    install_add_piece_handler(document, Rc::clone(&state))?;
    install_button(document, "start-button", Rc::clone(&state), |state| {
        let document = current_document()?;
        commit_pending_radius_change(state, &document, false)?;
        if state.running {
            return Ok(());
        }
        state.running = true;
        state.has_run = true;
        update_run_buttons(&document, state.running)?;
        set_status("Running")?;
        send_to_worker(&state.worker, &AppToWorker::Start)?;
        send_to_worker(&state.worker, &AppToWorker::RunTick)
    })?;
    install_button(document, "pause-button", Rc::clone(&state), |state| {
        let document = current_document()?;
        commit_pending_radius_change(state, &document, false)?;
        if !state.running {
            return Ok(());
        }
        state.running = false;
        state.has_run = true;
        update_run_buttons(&document, state.running)?;
        set_status("Paused")?;
        send_to_worker(&state.worker, &AppToWorker::Pause)
    })?;
    install_button(document, "step-button", Rc::clone(&state), |state| {
        let document = current_document()?;
        commit_pending_radius_change(state, &document, false)?;
        if state.running {
            return Ok(());
        }
        set_status("Stepping")?;
        send_to_worker(&state.worker, &AppToWorker::StepBatch { max_steps: 1 })
    })?;
    install_button(
        document,
        "download-png-button",
        Rc::clone(&state),
        |state| {
            let filename = download_filename(&state.settings, state.last_stats, "image", "png");
            state.renderer.download_image("image/png", &filename)
        },
    )?;
    install_button(
        document,
        "download-webp-button",
        Rc::clone(&state),
        |state| {
            let filename = download_filename(&state.settings, state.last_stats, "image", "webp");
            state.renderer.download_image("image/webp", &filename)
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
        if let Err(error) = sync_settings(&document, Rc::clone(&state), action.clone()) {
            log_error(&error);
        }
    });

    element.add_event_listener_with_callback("input", closure.as_ref().unchecked_ref())?;
    element.add_event_listener_with_callback("change", closure.as_ref().unchecked_ref())?;
    closure.forget();
    Ok(())
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

        state.borrow_mut().settings.custom_army.push(piece);

        if let Err(error) = render_army_list(&document, Rc::clone(&state)) {
            log_error(&error);
        }
        if let Err(error) = sync_settings(&document, Rc::clone(&state), SyncAction::ResetWorker) {
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

fn render_army_list(document: &Document, state: Rc<RefCell<AppState>>) -> Result<(), JsValue> {
    let list = document
        .get_element_by_id("army-list")
        .ok_or_else(|| JsValue::from_str("missing army-list"))?;
    list.set_inner_html("");

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
        label.set_text_content(Some(&format!(
            "{}. ({}, {}) gap {}",
            index + 1,
            piece.a,
            piece.b,
            piece_gap(piece.a, piece.b)
        )));
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

        let up = small_button(document, "Up", "Move up")?;
        install_piece_action(&up, document.clone(), Rc::clone(&state), move |army| {
            if index > 0 {
                army.swap(index, index - 1);
            }
        })?;
        row.append_child(&up)?;

        let down = small_button(document, "Dn", "Move down")?;
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

fn install_piece_drag_handlers(
    row: &Element,
    document: Document,
    state: Rc<RefCell<AppState>>,
    index: usize,
) -> Result<(), JsValue> {
    let drag_state = Rc::clone(&state);
    let drag_start = Closure::<dyn FnMut(DragEvent)>::new(move |event: DragEvent| {
        drag_state.borrow_mut().dragging_army_index = Some(index);
        if let Some(data_transfer) = event.data_transfer() {
            data_transfer.set_effect_allowed("move");
            let _ = data_transfer.set_data("text/plain", &index.to_string());
        }
    });
    row.add_event_listener_with_callback("dragstart", drag_start.as_ref().unchecked_ref())?;
    drag_start.forget();

    let drag_over = Closure::<dyn FnMut(DragEvent)>::new(move |event: DragEvent| {
        event.prevent_default();
        if let Some(data_transfer) = event.data_transfer() {
            data_transfer.set_drop_effect("move");
        }
    });
    row.add_event_listener_with_callback("dragover", drag_over.as_ref().unchecked_ref())?;
    drag_over.forget();

    let drop_document = document.clone();
    let drop_state = Rc::clone(&state);
    let drop = Closure::<dyn FnMut(DragEvent)>::new(move |event: DragEvent| {
        event.prevent_default();
        let moved = {
            let mut state = drop_state.borrow_mut();
            let Some(from) = state.dragging_army_index.take() else {
                return;
            };
            move_custom_piece(&mut state.settings.custom_army, from, index)
        };

        if moved {
            if let Err(error) = render_army_list(&drop_document, Rc::clone(&drop_state)) {
                log_error(&error);
            }
            if let Err(error) = sync_settings(
                &drop_document,
                Rc::clone(&drop_state),
                SyncAction::ResetWorker,
            ) {
                log_error(&error);
            }
        }
    });
    row.add_event_listener_with_callback("drop", drop.as_ref().unchecked_ref())?;
    drop.forget();

    let end_state = state;
    let drag_end = Closure::<dyn FnMut(DragEvent)>::new(move |_event: DragEvent| {
        end_state.borrow_mut().dragging_army_index = None;
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
        action(&mut state.borrow_mut().settings.custom_army);
        if let Err(error) = render_army_list(&document, Rc::clone(&state)) {
            log_error(&error);
        }
        if let Err(error) = sync_settings(&document, Rc::clone(&state), SyncAction::ResetWorker) {
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
) -> Result<(), JsValue> {
    let previous_settings = state.borrow().settings.clone();
    let custom_army = previous_settings.custom_army.clone();
    apply_board_defaults(document, &previous_settings)?;
    let settings = read_settings(document, custom_army)?;
    update_outputs(document, &settings)?;
    let action = resolve_sync_action(action, &previous_settings, &settings);

    {
        let mut state = state.borrow_mut();
        state.settings = settings.clone();
        state.renderer.set_settings(settings.clone())?;
    }
    render_army_list(document, Rc::clone(&state))?;

    match action {
        SyncAction::RenderOnly => {}
        SyncAction::AutoControl(_) => {}
        SyncAction::DebounceRadius => {
            schedule_radius_commit(document.clone(), Rc::clone(&state))?;
        }
        SyncAction::UpdateWorker => {
            let mut state = state.borrow_mut();
            let worker_settings = worker_visible_settings(&state);
            send_to_worker(
                &state.worker,
                &AppToWorker::UpdateSettings {
                    settings: worker_settings.clone(),
                },
            )?;
            state.worker_settings = worker_settings;
        }
        SyncAction::ResetWorker => {
            let mut state = state.borrow_mut();
            reset_worker_with_settings(&mut state, document, settings, false)?;
        }
    }

    Ok(())
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
            let was_running = state.running;
            if let Err(error) = commit_pending_radius_change(&mut state, &document, was_running) {
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
) -> Result<(), JsValue> {
    if !state.radius_commit_pending {
        return Ok(());
    }

    let settings = state.settings.clone();
    reset_worker_with_settings(state, document, settings, restart_after_reset)
}

fn reset_worker_with_settings(
    state: &mut AppState,
    document: &Document,
    settings: EngineSettings,
    restart_after_reset: bool,
) -> Result<(), JsValue> {
    state.radius_commit_pending = false;
    state.radius_commit_generation = state.radius_commit_generation.wrapping_add(1);
    state.running = restart_after_reset;
    state.has_run = restart_after_reset;
    state.last_stats = EngineStats::default();
    state.last_ui_refresh_ms = 0.0;
    state.render_scheduled = false;
    state.worker_settings = settings.clone();
    update_run_buttons(document, state.running)?;
    state.renderer.clear_placements()?;
    reset_placement_log(state)?;
    send_to_worker(&state.worker, &AppToWorker::Reset { settings })?;
    if restart_after_reset {
        send_to_worker(&state.worker, &AppToWorker::Start)?;
        send_to_worker(&state.worker, &AppToWorker::RunTick)?;
    }
    Ok(())
}

fn apply_board_defaults(document: &Document, previous: &EngineSettings) -> Result<(), JsValue> {
    let next_board = match select_value(document, "board-select")?.as_str() {
        "LatticeHex" => BoardKind::LatticeHex,
        "ContinuousArchimedean" => BoardKind::ContinuousArchimedean,
        _ => BoardKind::LatticeSquare,
    };

    if previous.board != next_board {
        let piece_radius = if next_board == BoardKind::ContinuousArchimedean {
            "0.25"
        } else {
            "0.50"
        };
        input(document, "piece-radius-slider")?.set_value(piece_radius);
    }

    Ok(())
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
) -> Result<EngineSettings, JsValue> {
    let board = match select_value(document, "board-select")?.as_str() {
        "LatticeHex" => BoardKind::LatticeHex,
        "ContinuousArchimedean" => BoardKind::ContinuousArchimedean,
        _ => BoardKind::LatticeSquare,
    };

    let shape = if board == BoardKind::ContinuousArchimedean {
        set_select_value(document, "shape-select", "Circle")?;
        ShapeKind::Circle
    } else {
        match select_value(document, "shape-select")?.as_str() {
            "Circle" => ShapeKind::Circle,
            _ => ShapeKind::Square,
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
        "PrimeGap" => ArmyPreset::PrimeGap,
        _ => ArmyPreset::CustomFinite,
    };

    let enemy_mode = match select_value(document, "enemy-mode-select")?.as_str() {
        "Color" => EnemyMode::Color,
        _ => EnemyMode::MoveSet,
    };

    Ok(EngineSettings {
        board,
        shape,
        radius: input_value(document, "radius-input")?
            .parse::<f64>()
            .unwrap_or(100.0)
            .max(1.0),
        piece_radius: input_value(document, "piece-radius-slider")?
            .parse::<f64>()
            .unwrap_or(if board == BoardKind::ContinuousArchimedean {
                0.25
            } else {
                0.5
            })
            .clamp(0.05, 0.5),
        speed,
        display_mode,
        zoom: input_value(document, "zoom-slider")?.parse().unwrap_or(4),
        proactive_attacking: input_checked(document, "attacking-toggle")?,
        enemy_mode,
        army_preset,
        custom_army: normalize_custom_army(custom_army),
        continuous_offset: input_value(document, "continuous-offset-input")?
            .parse::<f64>()
            .unwrap_or(0.0)
            .clamp(0.0, 1.0),
        prime_modulo_divisor: input_value(document, "prime-divisor-input")?
            .parse::<u32>()
            .unwrap_or(12)
            .max(2),
        anchor_color_a: input_value(document, "anchor-a-input")?,
        anchor_color_b: input_value(document, "anchor-b-input")?,
    })
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

    input(document, "speed-slider")?.set_disabled(matches!(settings.speed, SpeedMode::Fastest));
    let continuous = settings.board == BoardKind::ContinuousArchimedean;
    if continuous {
        set_select_value(document, "shape-select", "Circle")?;
    }
    select(document, "shape-select")?.set_disabled(continuous);
    input(document, "continuous-offset-input")?.set_disabled(!continuous);
    select(document, "enemy-mode-select")?.set_disabled(false);

    let zoom_row = html_element(document, "zoom-row")?;
    let zoom_display = if settings.display_mode == DisplayMode::PixelOneToOne {
        "grid"
    } else {
        "none"
    };
    zoom_row.style().set_property("display", zoom_display)?;
    set_text(
        document,
        "zoom-output",
        &format!("x{}", input_value(document, "zoom-slider")?),
    )?;

    let custom_display = if settings.army_preset == ArmyPreset::CustomFinite {
        "grid"
    } else {
        "none"
    };
    html_element(document, "custom-army-editor")?
        .style()
        .set_property("display", custom_display)?;

    let prime_display = "grid";
    html_element(document, "prime-color-controls")?
        .style()
        .set_property("display", prime_display)?;
    button(document, "add-piece-button")?
        .set_disabled(settings.army_preset != ArmyPreset::CustomFinite);
    input(document, "prime-divisor-input")?
        .set_disabled(settings.army_preset != ArmyPreset::PrimeKnight);
    input(document, "anchor-a-input")?.set_disabled(false);
    input(document, "anchor-b-input")?.set_disabled(false);

    Ok(())
}

fn update_run_buttons(document: &Document, running: bool) -> Result<(), JsValue> {
    button(document, "start-button")?.set_disabled(running);
    button(document, "pause-button")?.set_disabled(!running);
    button(document, "step-button")?.set_disabled(running);
    Ok(())
}

fn status_text(state: &AppState, stats: EngineStats) -> String {
    let exhausted = stats.exhausted;
    let stats = stats_text(&state.settings, stats);
    if exhausted {
        stats
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
                "{} placements | {} spot checks",
                stats.placements, stats.spots_tested
            )
        }
        ArmyPreset::PrimeKnight | ArmyPreset::PrimeGap => format!(
            "{} placements | {} prime candidates tested | {} skipped spots",
            stats.placements, stats.piece_candidates_tested, stats.skipped_spots
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

#[allow(dead_code)]
fn element(document: &Document, id: &str) -> Result<Element, JsValue> {
    document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("missing element #{id}")))
}

fn log_error(error: &JsValue) {
    web_sys::console::error_1(error);
}
