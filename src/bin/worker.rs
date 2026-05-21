#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

#[cfg(target_arch = "wasm32")]
use spiral_pattern_generator::engine::SimulationEngine;
#[cfg(target_arch = "wasm32")]
use spiral_pattern_generator::protocol::{
    AppToWorker, ArmyPreset, AttackOverlayUpdate, BoardKind, EngineSettings, Placement, SpeedMode,
    VertexBufferUpdate, WorkerToApp,
};
#[cfg(target_arch = "wasm32")]
use spiral_pattern_generator::render_data::{
    AttackOverlayBuildJob, AttackOverlayCache,
    attack_overlay_requires_full_rebuild_for_new_placements, pack_vertices,
};
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
    attack_overlay_cache: Option<AttackOverlayCache>,
    attack_overlay_job: Option<AttackOverlayBuildJob>,
    running: bool,
    epoch: u64,
}

#[cfg(target_arch = "wasm32")]
const ATTACK_OVERLAY_BUILD_CHUNK: usize = 4_096;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();

    let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
    let runtime = Rc::new(RefCell::new(WorkerRuntime {
        scope: scope.clone(),
        engine: SimulationEngine::new(EngineSettings::default()),
        placements: Vec::new(),
        attack_overlay_cache: None,
        attack_overlay_job: None,
        running: false,
        epoch: 0,
    }));

    let handler_runtime = Rc::clone(&runtime);
    let closure = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        if let Err(error) = handle_message(&handler_runtime, event) {
            let runtime = handler_runtime.borrow();
            post_worker_message(
                &runtime.scope,
                &WorkerToApp::Error {
                    epoch: runtime.epoch,
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
        AppToWorker::Initialize { epoch, settings } | AppToWorker::Reset { epoch, settings } => {
            let mut runtime = runtime.borrow_mut();
            runtime.epoch = epoch;
            runtime.running = false;
            runtime.engine.reset(settings);
            runtime.placements.clear();
            runtime.attack_overlay_cache = None;
            runtime.attack_overlay_job = None;
            post_stats_with_updates(
                &runtime,
                VertexBufferUpdate::Replace(Vec::new()),
                AttackOverlayUpdate::replace_empty(),
            );
        }
        AppToWorker::UpdateSettings { epoch, settings } => {
            let mut runtime = runtime.borrow_mut();
            if epoch != runtime.epoch {
                return Ok(());
            }
            let old_settings = runtime.engine.settings().clone();
            let anchor_colors_changed = old_settings.anchor_color_a != settings.anchor_color_a
                || old_settings.anchor_color_b != settings.anchor_color_b;
            let overlay_may_need_refresh = old_settings.attack_overlay_opacity > f32::EPSILON
                || settings.attack_overlay_opacity > f32::EPSILON;
            let reset = runtime.engine.update_settings(settings);
            if reset {
                runtime.placements.clear();
                runtime.attack_overlay_cache = None;
                runtime.attack_overlay_job = None;
                post_stats_with_updates(
                    &runtime,
                    VertexBufferUpdate::Replace(Vec::new()),
                    AttackOverlayUpdate::replace_empty(),
                );
            } else if anchor_colors_changed && !runtime.placements.is_empty() {
                let vertices = pack_vertices(
                    &runtime.placements,
                    runtime.engine.settings(),
                    runtime.engine.color_state(),
                );
                let attack_overlay_update =
                    attack_overlay_update_for_runtime(&mut runtime, overlay_may_need_refresh);
                post_stats_with_updates(
                    &runtime,
                    VertexBufferUpdate::Replace(vertices),
                    attack_overlay_update,
                );
            } else if overlay_may_need_refresh {
                let attack_overlay_update = attack_overlay_update_for_runtime(&mut runtime, true);
                post_stats_with_updates(&runtime, VertexBufferUpdate::None, attack_overlay_update);
            } else {
                post_stats(&runtime);
            }
        }
        AppToWorker::Start { epoch } => {
            let mut runtime = runtime.borrow_mut();
            if epoch == runtime.epoch {
                runtime.running = true;
            }
        }
        AppToWorker::Pause { epoch } => {
            let mut runtime = runtime.borrow_mut();
            if epoch != runtime.epoch {
                return Ok(());
            }
            runtime.running = false;
            post_stats(&runtime);
        }
        AppToWorker::RunTick { epoch } => {
            let mut runtime = runtime.borrow_mut();
            if epoch != runtime.epoch {
                return Ok(());
            }
            if runtime.running {
                if runtime.engine.settings().visual_progress {
                    let (batch_size, work_budget) = batch_parameters(runtime.engine.settings());
                    post_step_result(&mut runtime, batch_size, work_budget);
                } else {
                    post_finish_only_result(&mut runtime);
                }
            } else {
                post_stats(&runtime);
            }
        }
        AppToWorker::StepBatch { epoch, max_steps } => {
            let mut runtime = runtime.borrow_mut();
            if epoch != runtime.epoch {
                return Ok(());
            }
            post_step_result(&mut runtime, max_steps, 2_000_000);
        }
        AppToWorker::BuildAttackOverlay { epoch } => {
            let mut runtime = runtime.borrow_mut();
            if epoch != runtime.epoch {
                return Ok(());
            }
            post_attack_overlay_build_chunk(&mut runtime);
        }
    }

    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn post_finish_only_result(runtime: &mut WorkerRuntime) {
    const MAX_SILENT_WORK: u64 = 20_000_000;
    const MAX_EMPTY_LOOPS: u32 = 64;

    let mut silent_work = 0_u64;
    let mut empty_loops = 0_u32;

    while runtime.running
        && !runtime.engine.stats().exhausted
        && silent_work < MAX_SILENT_WORK
        && empty_loops < MAX_EMPTY_LOOPS
    {
        let previous = runtime.engine.stats();
        let (_, work_budget) = batch_parameters(runtime.engine.settings());
        let placements = runtime
            .engine
            .step_budget(16_384, work_budget.saturating_mul(8).max(1_000_000));
        let stats = runtime.engine.stats();
        let work_delta = stats
            .spots_tested
            .saturating_sub(previous.spots_tested)
            .saturating_add(
                stats
                    .piece_candidates_tested
                    .saturating_sub(previous.piece_candidates_tested),
            )
            .max(1);
        silent_work = silent_work.saturating_add(work_delta);

        if placements.is_empty() {
            empty_loops += 1;
        } else {
            empty_loops = 0;
            runtime.placements.extend_from_slice(&placements);
        }
    }

    let stats = runtime.engine.stats();
    let color_state = runtime.engine.color_state();
    if stats.exhausted {
        runtime.running = false;
        let vertices = pack_vertices(&runtime.placements, runtime.engine.settings(), color_state);
        let log_placements = log_sample_for_full_run(&runtime.placements);
        let attack_overlay_update = attack_overlay_update_for_runtime(runtime, false);
        post_worker_message(
            &runtime.scope,
            &WorkerToApp::Batch {
                epoch: runtime.epoch,
                log_placements,
                vertex_update: VertexBufferUpdate::Replace(vertices),
                attack_overlay_update,
                attack_overlay_pending: attack_overlay_pending(runtime),
                stats,
                color_state,
            },
        );
    } else {
        post_worker_message(
            &runtime.scope,
            &WorkerToApp::Stats {
                epoch: runtime.epoch,
                stats,
                color_state,
                vertex_update: VertexBufferUpdate::None,
                attack_overlay_update: AttackOverlayUpdate::none(),
                attack_overlay_pending: attack_overlay_pending(runtime),
            },
        );
    }
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
                epoch: runtime.epoch,
                stats,
                color_state,
                vertex_update: VertexBufferUpdate::None,
                attack_overlay_update: AttackOverlayUpdate::none(),
                attack_overlay_pending: attack_overlay_pending(runtime),
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
        let attack_overlay_update = attack_overlay_update_for_new_placements(runtime, &placements);
        let log_placements = log_sample_for_batch(previous_placement_count, &placements);
        post_worker_message(
            &runtime.scope,
            &WorkerToApp::Batch {
                epoch: runtime.epoch,
                log_placements,
                vertex_update,
                attack_overlay_update,
                attack_overlay_pending: attack_overlay_pending(runtime),
                stats,
                color_state,
            },
        );
    }
}

#[cfg(target_arch = "wasm32")]
fn post_stats(runtime: &WorkerRuntime) {
    post_stats_with_updates(
        runtime,
        VertexBufferUpdate::None,
        AttackOverlayUpdate::none(),
    );
}

#[cfg(target_arch = "wasm32")]
fn post_stats_with_updates(
    runtime: &WorkerRuntime,
    vertex_update: VertexBufferUpdate,
    attack_overlay_update: AttackOverlayUpdate,
) {
    post_worker_message(
        &runtime.scope,
        &WorkerToApp::Stats {
            epoch: runtime.epoch,
            stats: runtime.engine.stats(),
            color_state: runtime.engine.color_state(),
            vertex_update,
            attack_overlay_update,
            attack_overlay_pending: attack_overlay_pending(runtime),
        },
    );
}

#[cfg(target_arch = "wasm32")]
fn attack_overlay_update_for_runtime(
    runtime: &mut WorkerRuntime,
    force_clear_when_disabled: bool,
) -> AttackOverlayUpdate {
    if runtime.engine.settings().attack_overlay_opacity <= f32::EPSILON {
        runtime.attack_overlay_cache = None;
        runtime.attack_overlay_job = None;
        return if force_clear_when_disabled {
            AttackOverlayUpdate::replace_empty()
        } else {
            AttackOverlayUpdate::none()
        };
    }

    runtime.attack_overlay_cache = None;
    runtime.attack_overlay_job = Some(AttackOverlayBuildJob::new(
        runtime.engine.settings().clone(),
        runtime.placements.len(),
    ));
    AttackOverlayUpdate::replace_empty()
}

#[cfg(target_arch = "wasm32")]
fn attack_overlay_update_for_new_placements(
    runtime: &mut WorkerRuntime,
    placements: &[Placement],
) -> AttackOverlayUpdate {
    if runtime.engine.settings().attack_overlay_opacity <= f32::EPSILON {
        runtime.attack_overlay_cache = None;
        runtime.attack_overlay_job = None;
        return AttackOverlayUpdate::none();
    }

    if runtime.attack_overlay_job.is_some() {
        return AttackOverlayUpdate::none();
    }

    if attack_overlay_requires_full_rebuild_for_new_placements(runtime.engine.settings()) {
        runtime.attack_overlay_cache = None;
        runtime.attack_overlay_job = Some(AttackOverlayBuildJob::new(
            runtime.engine.settings().clone(),
            runtime.placements.len(),
        ));
        return AttackOverlayUpdate::replace_empty();
    }

    if runtime.attack_overlay_cache.is_none() {
        if runtime.placements.len() > placements.len() {
            runtime.attack_overlay_job = Some(AttackOverlayBuildJob::new(
                runtime.engine.settings().clone(),
                runtime.placements.len(),
            ));
            return AttackOverlayUpdate::replace_empty();
        }
        runtime.attack_overlay_cache =
            Some(AttackOverlayCache::new(runtime.engine.settings().clone()));
    }

    runtime
        .attack_overlay_cache
        .as_mut()
        .map_or_else(AttackOverlayUpdate::none, |cache| {
            cache.append_placements(placements)
        })
}

#[cfg(target_arch = "wasm32")]
fn post_attack_overlay_build_chunk(runtime: &mut WorkerRuntime) {
    if runtime.engine.settings().attack_overlay_opacity <= f32::EPSILON {
        runtime.attack_overlay_cache = None;
        runtime.attack_overlay_job = None;
        post_stats_with_updates(
            runtime,
            VertexBufferUpdate::None,
            AttackOverlayUpdate::replace_empty(),
        );
        return;
    }

    let Some(mut job) = runtime.attack_overlay_job.take() else {
        post_stats(runtime);
        return;
    };

    let (mut update, pending) = job.process_chunk(&runtime.placements, ATTACK_OVERLAY_BUILD_CHUNK);
    if pending {
        runtime.attack_overlay_job = Some(job);
    } else {
        let mut cache = job.into_cache();
        let synced = cache.placement_count();
        if runtime.placements.len() > synced {
            if attack_overlay_requires_full_rebuild_for_new_placements(runtime.engine.settings()) {
                runtime.attack_overlay_cache = None;
                runtime.attack_overlay_job = Some(AttackOverlayBuildJob::new(
                    runtime.engine.settings().clone(),
                    runtime.placements.len(),
                ));
                update = AttackOverlayUpdate::replace_empty();
            } else {
                let extra = cache.append_placements(&runtime.placements[synced..]);
                update = merge_attack_overlay_updates(update, extra);
                runtime.attack_overlay_cache = Some(cache);
            }
        } else {
            runtime.attack_overlay_cache = Some(cache);
        }
    }

    post_stats_with_updates(runtime, VertexBufferUpdate::None, update);
}

#[cfg(target_arch = "wasm32")]
fn attack_overlay_pending(runtime: &WorkerRuntime) -> bool {
    runtime.attack_overlay_job.is_some()
}

#[cfg(target_arch = "wasm32")]
fn merge_attack_overlay_updates(
    first: AttackOverlayUpdate,
    second: AttackOverlayUpdate,
) -> AttackOverlayUpdate {
    AttackOverlayUpdate {
        spots: merge_vertex_updates(first.spots, second.spots),
        hits: merge_vertex_updates(first.hits, second.hits),
        circles: merge_vertex_updates(first.circles, second.circles),
    }
}

#[cfg(target_arch = "wasm32")]
fn merge_vertex_updates(
    first: VertexBufferUpdate,
    second: VertexBufferUpdate,
) -> VertexBufferUpdate {
    match (first, second) {
        (VertexBufferUpdate::None, update) | (update, VertexBufferUpdate::None) => update,
        (VertexBufferUpdate::Append(mut a), VertexBufferUpdate::Append(b)) => {
            a.extend_from_slice(&b);
            VertexBufferUpdate::Append(a)
        }
        (_, replace @ VertexBufferUpdate::Replace(_)) => replace,
        (replace @ VertexBufferUpdate::Replace(_), _) => replace,
    }
}

#[cfg(target_arch = "wasm32")]
fn batch_parameters(settings: &EngineSettings) -> (u32, u64) {
    match settings.speed {
        SpeedMode::Fastest => match (settings.board, settings.army_preset) {
            (
                BoardKind::ContinuousArchimedean,
                ArmyPreset::PrimeKnight | ArmyPreset::PrimeGapper,
            ) => (64, 200_000),
            (_, ArmyPreset::PrimeKnight | ArmyPreset::PrimeGapper) => (512, 500_000),
            _ => (4_096, 1_000_000),
        },
        SpeedMode::PerSecond(rate) => {
            let rate = (rate as u32).max(1);
            let batch = if rate <= 20 { 1 } else { rate.div_ceil(20) };
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
fn log_sample_for_full_run(placements: &[Placement]) -> Vec<Placement> {
    const LOG_EDGE_COUNT: usize = 32;

    if placements.len() <= LOG_EDGE_COUNT * 2 {
        return placements.to_vec();
    }

    let mut out = Vec::with_capacity(LOG_EDGE_COUNT * 2);
    out.extend_from_slice(&placements[..LOG_EDGE_COUNT]);
    out.extend_from_slice(&placements[placements.len() - LOG_EDGE_COUNT..]);
    out
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
