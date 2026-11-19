#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod node;
mod policy;
mod state;
mod updates;
use serde_json::{Value, json};
use state::{DesktopState, PendingLogin};
use tauri::{Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_notification::NotificationExt;
type Outcome = Result<Value, &'static str>;
fn guard(window: &WebviewWindow) -> Result<(), &'static str> {
    if policy::trusted_companion(
        window.label(),
        &window.url().map_err(|_| "untrusted window")?,
    ) {
        Ok(())
    } else {
        Err("untrusted IPC origin")
    }
}
pub(crate) fn confirm(app: &tauri::AppHandle, message: &str) -> bool {
    app.dialog()
        .message(message)
        .title("AgentForge native approval")
        .buttons(MessageDialogButtons::OkCancel)
        .blocking_show()
}
fn notify(app: &tauri::AppHandle, message: &str) {
    // Only fixed native messages are allowed. Remote content never controls
    // notification text, links or actions. Permission failures are nonfatal.
    let _ = app
        .notification()
        .builder()
        .title("AgentForge Companion")
        .body(message)
        .show();
}
#[tauri::command]
async fn configure_gateway(
    window: WebviewWindow,
    app: tauri::AppHandle,
    state: tauri::State<'_, DesktopState>,
    origin: String,
) -> Outcome {
    guard(&window)?;
    let _operation = state.operations.lock().await;
    let origin = policy::gateway(&origin)?.to_string();
    if !confirm(
        &app,
        &format!(
            "Use {origin} for native device authentication and Console? Verify this address belongs to your organization."
        ),
    ) {
        return Err("Gateway change cancelled");
    }
    let mut prefs = state.preferences.lock().await;
    let candidate = state::Preferences {
        gateway: Some(origin.clone()),
        cohort: prefs.cohort.clone(),
    };
    state::atomic_json(&state.directory.join("preferences.json"), &candidate)?;
    *prefs = candidate;
    *state.pending.lock().await = None;
    // Destroy all previous-origin Console webviews before the new origin is
    // eligible. Browser cookies stay isolated by origin; native vault entries
    // are already keyed by exact origin in platform-client.
    if let Some(console) = app.get_webview_window("console") {
        console
            .close()
            .map_err(|_| "previous Console could not close")?;
    }
    Ok(json!({"gateway":origin,"native_session":"origin_scoped"}))
}
#[tauri::command]
async fn desktop_status(window: WebviewWindow, state: tauri::State<'_, DesktopState>) -> Outcome {
    guard(&window)?;
    let _operation = state.operations.lock().await;
    let gateway = state.preferences.lock().await.gateway.clone();
    let reachability = if let Some(origin) = &gateway {
        match state.http.head(origin).send().await {
            Ok(r) if !r.status().is_redirection() => "reachable",
            _ => "offline_or_unreachable",
        }
    } else {
        "unconfigured"
    };
    let auth = match state.login().await {
        Ok(login) => login
            .status()
            .map_err(|_| "native session status unavailable")?,
        Err(_) => json!({"authenticated":false,"vault":"unavailable_or_unconfigured"}),
    };
    Ok(
        json!({"gateway":gateway,"gateway_reachability":reachability,"native_session":auth,"console_session":"independent_http_only_cookie","local_node_control":"not_observed","offline_mutations":"disabled"}),
    )
}
#[tauri::command]
async fn open_console(
    window: WebviewWindow,
    app: tauri::AppHandle,
    state: tauri::State<'_, DesktopState>,
    offline: bool,
) -> Outcome {
    guard(&window)?;
    let _operation = state.operations.lock().await;
    let label = if offline {
        "console-offline"
    } else {
        "console"
    };
    if let Some(console) = app.get_webview_window(label) {
        console.show().map_err(|_| "Console unavailable")?;
        console.set_focus().map_err(|_| "Console unavailable")?;
        return Ok(json!({"opened":true,"offline":offline}));
    }
    if offline {
        WebviewWindowBuilder::new(&app, label, WebviewUrl::App("index.html".into()))
            .title("AgentForge Console — offline preview")
            .inner_size(1280., 860.)
            .on_navigation(|url| {
                matches!(url.scheme(), "tauri" | "http")
                    && matches!(url.host_str(), Some("localhost" | "tauri.localhost"))
                    && !url.path().starts_with("/native/")
            })
            .build()
            .map_err(|_| "offline Console unavailable")?;
    } else {
        let raw = state
            .preferences
            .lock()
            .await
            .gateway
            .clone()
            .ok_or("configure Gateway first")?;
        let origin = policy::gateway(&raw)?;
        let pinned = origin.clone();
        // Gateway serves this same Console build and its own CSP/Trusted Types,
        // HttpOnly cookies, CSRF and SSE. No cross-origin credential bridge.
        WebviewWindowBuilder::new(&app, label, WebviewUrl::External(origin))
            .title("AgentForge Console")
            .inner_size(1280., 860.)
            .on_navigation(move |url| policy::console_navigation(&pinned, url))
            .build()
            .map_err(|_| "Console unavailable")?;
    }
    Ok(json!({"opened":true,"offline":offline,"native_ipc":false}))
}
#[tauri::command]
async fn begin_login(
    window: WebviewWindow,
    app: tauri::AppHandle,
    state: tauri::State<'_, DesktopState>,
) -> Outcome {
    guard(&window)?;
    let _operation = state.operations.lock().await;
    if state.pending.lock().await.is_some() {
        return Err("device sign-in already pending");
    }
    let origin = state
        .preferences
        .lock()
        .await
        .gateway
        .clone()
        .ok_or("configure Gateway first")?;
    if !confirm(
        &app,
        &format!(
            "Start native device sign-in at {origin}? The approval code will be displayed only in the companion."
        ),
    ) {
        return Err("device sign-in cancelled");
    }
    let login = state.login().await?;
    let verifier = ai_platform_client::device_auth::DeviceLogin::verifier();
    let challenge = login
        .begin(&verifier)
        .await
        .map_err(|_| "device authorization outcome not confirmed")?;
    let result = json!({"user_code":challenge.user_code,"verification_uri":challenge.verification_uri,"expires_in":challenge.expires_in});
    *state.pending.lock().await = Some(PendingLogin {
        challenge,
        verifier,
    });
    Ok(result)
}
#[tauri::command]
async fn finish_login(
    window: WebviewWindow,
    app: tauri::AppHandle,
    state: tauri::State<'_, DesktopState>,
) -> Outcome {
    guard(&window)?;
    let _operation = state.operations.lock().await;
    let pending = state
        .pending
        .lock()
        .await
        .take()
        .ok_or("no pending device sign-in")?;
    let result = state
        .login()
        .await?
        .complete(pending.challenge, &pending.verifier)
        .await
        .map_err(|_| "device sign-in outcome not confirmed; inspect native session status")?;
    notify(&app, "Native device sign-in completed.");
    Ok(result)
}
#[tauri::command]
async fn native_identity(window: WebviewWindow, state: tauri::State<'_, DesktopState>) -> Outcome {
    guard(&window)?;
    let _operation = state.operations.lock().await;
    let origin = state
        .preferences
        .lock()
        .await
        .gateway
        .clone()
        .ok_or("configure Gateway first")?;
    // Refresh rotation and its durable unknown-outcome fence live in the shared
    // client. Only a real authenticated Gateway read can mark server verification.
    let access =
        state.login().await?.access_token().await.map_err(
            |_| "native authentication or refresh outcome not confirmed; inspect status",
        )?;
    let client = ai_platform_client::Client::new(&origin)
        .map_err(|_| "invalid Gateway origin")?
        .authenticated(access.token);
    client
        .me()
        .await
        .map_err(|_| "native session not verified by Gateway")?;
    Ok(
        json!({"authenticated":true,"server_verified":true,"tenant_id":access.tenant_id,"user_id":access.user_id,"expires_at":access.expires_at}),
    )
}
#[tauri::command]
async fn logout(
    window: WebviewWindow,
    app: tauri::AppHandle,
    state: tauri::State<'_, DesktopState>,
) -> Outcome {
    guard(&window)?;
    let _operation = state.operations.lock().await;
    if !confirm(
        &app,
        "Revoke the native session for this Gateway? This also signs out platformctl when it uses the same origin and registered public-client ID. Console cookies have their own sign-out.",
    ) {
        return Err("native sign-out cancelled");
    }
    *state.pending.lock().await = None;
    state
        .login()
        .await?
        .logout()
        .await
        .map_err(|_| "local native session cleared or revocation outcome unknown; inspect status")
}
#[tauri::command]
async fn pair_node(
    window: WebviewWindow,
    app: tauri::AppHandle,
    state: tauri::State<'_, DesktopState>,
    challenge_id: String,
    user_code: String,
) -> Outcome {
    guard(&window)?;
    let _operation = state.operations.lock().await;
    let origin = state
        .preferences
        .lock()
        .await
        .gateway
        .clone()
        .ok_or("configure Gateway first")?;
    if !confirm(
        &app,
        &format!(
            "Redeem this one-use pairing challenge at {origin} using the startup operator configuration and its exact workspace grants?"
        ),
    ) {
        return Err("pairing cancelled");
    }
    let result = node::pair(&state, &origin, challenge_id, user_code).await?;
    notify(&app, "Local Node pairing completed.");
    Ok(result)
}
#[tauri::command]
async fn node_status(window: WebviewWindow, state: tauri::State<'_, DesktopState>) -> Outcome {
    guard(&window)?;
    let _operation = state.operations.lock().await;
    let origin = state
        .preferences
        .lock()
        .await
        .gateway
        .clone()
        .ok_or("configure Gateway first")?;
    node::status(&state, &origin)
}
#[tauri::command]
async fn check_update(
    window: WebviewWindow,
    app: tauri::AppHandle,
    state: tauri::State<'_, DesktopState>,
) -> Outcome {
    guard(&window)?;
    let _operation = state.operations.lock().await;
    updates::check(&app, &state, false).await
}
#[tauri::command]
async fn install_update(
    window: WebviewWindow,
    app: tauri::AppHandle,
    state: tauri::State<'_, DesktopState>,
) -> Outcome {
    guard(&window)?;
    let _operation = state.operations.lock().await;
    updates::check(&app, &state, true).await
}
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            configure_gateway,
            desktop_status,
            open_console,
            begin_login,
            finish_login,
            logout,
            native_identity,
            pair_node,
            node_status,
            check_update,
            install_update
        ])
        .setup(|app| {
            let state =
                DesktopState::new(app.path().app_config_dir()?).map_err(std::io::Error::other)?;
            app.manage(state);
            let show =
                tauri::menu::MenuItem::with_id(app, "show", "Open companion", true, None::<&str>)?;
            let quit =
                tauri::menu::MenuItem::with_id(app, "quit", "Quit companion", true, None::<&str>)?;
            let menu = tauri::menu::Menu::with_items(app, &[&show, &quit])?;
            // Fixed RGBA tray glyph is source data, not an external executable or
            // remote image. Closing the companion window does not grant a node.
            let icon =
                tauri::image::Image::new_owned(vec![90, 90, 210, 255].repeat(16 * 16), 16, 16);
            tauri::tray::TrayIconBuilder::new()
                .icon(icon)
                .tooltip("AgentForge Companion")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("companion") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("desktop startup failed");
}
