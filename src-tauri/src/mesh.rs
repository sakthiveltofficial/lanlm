//! Commands for the cluster screen. They are thin: everything real happens in the `lanlm-mesh`
//! crate, which the headless `lanlm-node` program uses too.

use std::time::Duration;

use lanlm_mesh::control::{Controller, GatewaySettings, Status};
use lanlm_mesh::planner::PlanResult;
use lanlm_mesh::proto::{Command, Deployment};
use serde_json::Value;
use tauri::State;

use crate::AppState;

fn ctl<'a>(state: &'a State<'_, AppState>) -> Result<&'a Controller, String> {
    state
        .ctl
        .get()
        .ok_or_else(|| "The cluster service is not running".to_string())
}

#[tauri::command]
pub async fn mesh_status(state: State<'_, AppState>) -> Result<Status, String> {
    Ok(ctl(&state)?.status().await)
}

#[tauri::command]
pub fn mesh_create(state: State<'_, AppState>, name: String) -> Result<String, String> {
    Ok(ctl(&state)?.create_cluster(name.trim()))
}

#[tauri::command]
pub fn mesh_join(state: State<'_, AppState>, key: String) -> Result<(), String> {
    ctl(&state)?.join_cluster(&key)
}

#[tauri::command]
pub async fn mesh_leave(state: State<'_, AppState>) -> Result<(), String> {
    ctl(&state)?.leave_cluster().await;
    Ok(())
}

#[tauri::command]
pub fn mesh_key(state: State<'_, AppState>) -> Result<Option<String>, String> {
    Ok(ctl(&state)?.cluster_key())
}

#[tauri::command]
pub fn mesh_set_name(state: State<'_, AppState>, name: String) -> Result<(), String> {
    ctl(&state)?.mesh().set_name(&name);
    Ok(())
}

#[tauri::command]
pub fn mesh_add_peer(state: State<'_, AppState>, addr: String) -> Result<(), String> {
    ctl(&state)?.mesh().add_peer(&addr);
    Ok(())
}

#[tauri::command]
pub fn mesh_pair_open(state: State<'_, AppState>) -> Result<(), String> {
    ctl(&state)?.mesh().open_pairing();
    Ok(())
}

#[tauri::command]
pub fn mesh_pair_close(state: State<'_, AppState>) -> Result<(), String> {
    ctl(&state)?.mesh().close_pairing();
    Ok(())
}

#[tauri::command]
pub fn mesh_pair_decide(state: State<'_, AppState>, id: u64, allow: bool) -> Result<bool, String> {
    Ok(ctl(&state)?.mesh().pair_decide(id, allow))
}

#[tauri::command]
pub fn mesh_pair_start(state: State<'_, AppState>, addr: String) -> Result<(), String> {
    ctl(&state)?.mesh().pair_start(addr);
    Ok(())
}

#[tauri::command]
pub fn mesh_pair_reset(state: State<'_, AppState>) -> Result<(), String> {
    ctl(&state)?.mesh().pair_reset();
    Ok(())
}

#[tauri::command]
pub fn mesh_deploy(state: State<'_, AppState>, deployment: Deployment) -> Result<String, String> {
    ctl(&state)?.deploy(deployment)
}

#[tauri::command]
pub async fn mesh_undeploy(state: State<'_, AppState>, id: String) -> Result<(), String> {
    ctl(&state)?.undeploy(&id).await
}

#[tauri::command]
pub fn mesh_preview(
    state: State<'_, AppState>,
    model: String,
    mode: String,
    ctx: u32,
    parallel: u32,
    max: u32,
) -> Result<PlanResult, String> {
    Ok(ctl(&state)?.preview(&model, &mode, ctx, parallel, max))
}

/// Copies a model onto a device (empty `node` is this one), from a neighbour if one has it.
#[tauri::command]
pub async fn mesh_fetch(
    state: State<'_, AppState>,
    node: String,
    file: String,
    url: Option<String>,
) -> Result<(), String> {
    ctl(&state)?
        .fetch_model(&node, &file, url.filter(|u| !u.is_empty()), None)
        .await
}

#[tauri::command]
pub async fn mesh_cancel_fetch(
    state: State<'_, AppState>,
    node: String,
    file: String,
) -> Result<(), String> {
    ctl(&state)?
        .command(
            &node,
            Command::CancelFetch { file },
            Duration::from_secs(30),
        )
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn mesh_delete_model(
    state: State<'_, AppState>,
    node: String,
    file: String,
) -> Result<(), String> {
    ctl(&state)?
        .command(
            &node,
            Command::DeleteModel { file },
            Duration::from_secs(60),
        )
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn mesh_bench(state: State<'_, AppState>, node: String) -> Result<Value, String> {
    ctl(&state)?
        .command(&node, Command::Bench, Duration::from_secs(400))
        .await
}

#[tauri::command]
pub async fn mesh_drain(state: State<'_, AppState>, node: String, on: bool) -> Result<(), String> {
    ctl(&state)?
        .command(&node, Command::SetDraining { on }, Duration::from_secs(30))
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn mesh_lend(state: State<'_, AppState>, node: String, on: bool) -> Result<(), String> {
    ctl(&state)?
        .command(&node, Command::SetLend { on }, Duration::from_secs(30))
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn mesh_gateway(
    state: State<'_, AppState>,
    settings: GatewaySettings,
) -> Result<(), String> {
    let c = ctl(&state)?;
    if settings.enabled {
        c.gateway_start(settings).await.map(|_| ())
    } else {
        c.gateway_stop().await
    }
}

#[tauri::command]
pub fn mesh_logs(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    Ok(ctl(&state)?.node().logs())
}
