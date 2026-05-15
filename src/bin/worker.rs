#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

#[cfg(target_arch = "wasm32")]
use spiral_pattern_generator::engine::SimulationEngine;
#[cfg(target_arch = "wasm32")]
use spiral_pattern_generator::protocol::{
    AppToWorker, EngineSettings, Placement, SpeedMode, VertexBufferUpdate, WorkerToApp,
};
#[cfg(target_arch = "wasm32")]
use spiral_pattern_generator::render_data::pack_vertices;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;
#[cfg(target_arch = "wasm32")]
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

#[cfg(target_arch = "wasm32")]
struct WorkerRuntime {
    scope: DedicatedWorkerGlobalScope,
    engine: SimulationEngine,
    placements: Vec<Placement>,
    running: bool,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();

    let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
    let runtime = Rc::new(RefCell::new(WorkerRuntime {
        scope: scope.clone(),
        engine: SimulationEngine::new(EngineSettings::default()),
        placements: Vec::new(),
        running: false,
    }));

    let handler_runtime = Rc::clone(&runtime);
    let closure = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        if let Err(error) = handle_message(&handler_runtime, event) {
            let scope = handler_runtime.borrow().scope.clone();
            post_worker_message(
                &scope,
                &WorkerToApp::Error {
                    message: format!("{error:?}"),
                },
            );
        }
    });

    scope.set_onmessage(Some(closure.as_ref().unchecked_ref()));
    closure.forget();
    post_worker_message(&scope, &WorkerToApp::Ready);
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn handle_message(
    runtime: &Rc<RefCell<WorkerRuntime>>,
    event: MessageEvent,
) -> Result<(), JsValue> {
    let msg = decode_app_message(event)?;
    match msg {
        AppToWorker::Initialize { settings } | AppToWorker::Reset { settings } => {
            let mut runtime = runtime.borrow_mut();
            runtime.running = false;
            runtime.engine.reset(settings);
            runtime.placements.clear();
            post_stats(&runtime);
        }
        AppToWorker::UpdateSettings { settings } => {
            let mut runtime = runtime.borrow_mut();
            let old_settings = runtime.engine.settings().clone();
            let anchor_colors_changed = old_settings.anchor_color_a != settings.anchor_color_a
                || old_settings.anchor_color_b != settings.anchor_color_b;
            let reset = runtime.engine.update_settings(settings);
            if reset {
                runtime.placements.clear();
                post_stats_with_vertex_update(&runtime, VertexBufferUpdate::Replace(Vec::new()));
            } else if anchor_colors_changed && !runtime.placements.is_empty() {
                let vertices = pack_vertices(
                    &runtime.placements,
                    runtime.engine.settings(),
                    runtime.engine.color_state(),
                );
                post_stats_with_vertex_update(&runtime, VertexBufferUpdate::Replace(vertices));
            } else {
                post_stats(&runtime);
            }
        }
        AppToWorker::Start => {
            runtime.borrow_mut().running = true;
        }
        AppToWorker::Pause => {
            let mut runtime = runtime.borrow_mut();
            runtime.running = false;
            post_stats(&runtime);
        }
        AppToWorker::RunTick => {
            let mut runtime = runtime.borrow_mut();
            if runtime.running {
                let (batch_size, work_budget) = batch_parameters(&runtime.engine.settings().speed);
                post_step_result(&mut runtime, batch_size, work_budget);
            } else {
                post_stats(&runtime);
            }
        }
        AppToWorker::StepBatch { max_steps } => {
            let mut runtime = runtime.borrow_mut();
            post_step_result(&mut runtime, max_steps, 2_000_000);
        }
    }

    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn post_step_result(runtime: &mut WorkerRuntime, max_steps: u32, work_budget: u64) {
    let old_color_state = runtime.engine.color_state();
    let had_existing_placements = !runtime.placements.is_empty();
    let previous_placement_count = runtime.engine.stats().placements;
    let placements = runtime.engine.step_budget(max_steps, work_budget);
    let stats = runtime.engine.stats();
    let color_state = runtime.engine.color_state();
    if placements.is_empty() {
        post_worker_message(
            &runtime.scope,
            &WorkerToApp::Stats {
                stats,
                color_state,
                vertex_update: VertexBufferUpdate::None,
            },
        );
    } else {
        runtime.placements.extend_from_slice(&placements);
        let vertex_update = if had_existing_placements && color_state != old_color_state {
            VertexBufferUpdate::Replace(pack_vertices(
                &runtime.placements,
                runtime.engine.settings(),
                color_state,
            ))
        } else {
            VertexBufferUpdate::Append(pack_vertices(
                &placements,
                runtime.engine.settings(),
                color_state,
            ))
        };
        let log_placements = log_sample_for_batch(previous_placement_count, &placements);
        post_worker_message(
            &runtime.scope,
            &WorkerToApp::Batch {
                log_placements,
                vertex_update,
                stats,
                color_state,
            },
        );
    }
}

#[cfg(target_arch = "wasm32")]
fn post_stats(runtime: &WorkerRuntime) {
    post_stats_with_vertex_update(runtime, VertexBufferUpdate::None);
}

#[cfg(target_arch = "wasm32")]
fn post_stats_with_vertex_update(runtime: &WorkerRuntime, vertex_update: VertexBufferUpdate) {
    post_worker_message(
        &runtime.scope,
        &WorkerToApp::Stats {
            stats: runtime.engine.stats(),
            color_state: runtime.engine.color_state(),
            vertex_update,
        },
    );
}

#[cfg(target_arch = "wasm32")]
fn batch_parameters(speed: &SpeedMode) -> (u32, u64) {
    match speed {
        SpeedMode::Fastest => (4_096, 1_000_000),
        SpeedMode::PerSecond(rate) => {
            let frames_per_second = 20;
            let batch = ((*rate as u32).max(1) / frames_per_second).max(1);
            (batch, 20_000)
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn log_sample_for_batch(previous_placement_count: u64, placements: &[Placement]) -> Vec<Placement> {
    const LOG_EDGE_COUNT: usize = 32;

    if placements.is_empty() {
        return Vec::new();
    }

    let mut sample_indices = Vec::with_capacity(LOG_EDGE_COUNT * 2);

    if previous_placement_count < LOG_EDGE_COUNT as u64 {
        let missing_first = (LOG_EDGE_COUNT as u64 - previous_placement_count) as usize;
        let first_count = missing_first.min(placements.len());
        sample_indices.extend(0..first_count);
    }

    let tail_start = placements.len().saturating_sub(LOG_EDGE_COUNT);
    for index in tail_start..placements.len() {
        if !sample_indices.contains(&index) {
            sample_indices.push(index);
        }
    }

    sample_indices
        .into_iter()
        .map(|index| placements[index].clone())
        .collect()
}

#[cfg(target_arch = "wasm32")]
fn post_worker_message(scope: &DedicatedWorkerGlobalScope, msg: &WorkerToApp) {
    match bincode::serialize(msg) {
        Ok(bytes) => {
            let bytes = js_sys::Uint8Array::from(bytes.as_slice());
            let transfer = js_sys::Array::new();
            transfer.push(&bytes.buffer());
            if let Err(error) = scope.post_message_with_transfer(&bytes, &transfer) {
                web_sys::console::error_1(&error);
            }
        }
        Err(error) => {
            web_sys::console::error_1(&JsValue::from_str(&format!(
                "failed to encode worker message: {error}"
            )));
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn decode_app_message(event: MessageEvent) -> Result<AppToWorker, JsValue> {
    let bytes = js_sys::Uint8Array::new(&event.data());
    let mut buffer = vec![0_u8; bytes.length() as usize];
    bytes.copy_to(&mut buffer);
    bincode::deserialize::<AppToWorker>(&buffer)
        .map_err(|error| JsValue::from_str(&format!("failed to decode app message: {error}")))
}

fn main() {}
