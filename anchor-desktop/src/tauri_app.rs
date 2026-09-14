//! Tauri host for the next-generation Anchor desktop interface.

use crate::ui_bridge::{
    DesktopController, DesktopState, DroppedFileImport, ScreenPreview, encode_screen_preview,
    import_dropped_files as import_files_into_shared_folder,
};
use std::sync::Arc;
use tauri::{
    Emitter, Manager,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};

type ControllerState<'a> = tauri::State<'a, Arc<DesktopController>>;

// Keep Tauri's public command names explicit while avoiding a second, brittle
// copy of every simple `Result<(), String>` controller forwarder. Commands
// that transform arguments, return data, or do blocking work stay handwritten.
macro_rules! controller_action {
    ($name:ident($($argument:ident: $ty:ty),* $(,)?) => $method:ident($($value:expr),* $(,)?)) => {
        #[tauri::command]
        fn $name(controller: ControllerState<'_>, $($argument: $ty),*) -> Result<(), String> {
            controller.$method($($value),*)
        }
    };
}

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let icon = tauri::image::Image::from_bytes(include_bytes!("../../assets/anchor_logo_64.png"))?;

    let tray = TrayIconBuilder::with_id("anchor-tray").menu(&menu).icon(icon).tooltip("Anchor");

    tray.on_menu_event(|app, event| match event.id().as_ref() {
        "show" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }
        "quit" => app.exit(0),
        _ => {}
    })
    .build(app)?;

    Ok(())
}

fn setup_window_icon(app: &tauri::App) -> tauri::Result<()> {
    let icon = tauri::image::Image::from_bytes(include_bytes!("../../assets/anchor_logo_64.png"))?;
    if let Some(window) = app.get_webview_window("main") {
        window.set_icon(icon)?;
    }
    Ok(())
}

#[tauri::command]
fn desktop_state(controller: ControllerState<'_>, section: Option<String>) -> DesktopState {
    controller.state(section.as_deref())
}

controller_action!(set_streaming(enabled: bool) => set_streaming(enabled));
controller_action!(select_output(index: usize) => select_output(index));
controller_action!(create_virtual_display(width: u32, height: u32) => create_virtual_display(width, height));
controller_action!(destroy_virtual_display(name: String) => destroy_virtual_display(&name));

#[tauri::command]
async fn screen_preview(
    controller: ControllerState<'_>,
    after_sequence: u64,
) -> Result<Option<ScreenPreview>, String> {
    let Some(frame) = controller.screen_preview_frame(after_sequence) else {
        return Ok(None);
    };
    tauri::async_runtime::spawn_blocking(move || encode_screen_preview(frame))
        .await
        .map_err(|error| format!("Could not prepare screen preview: {error}"))?
        .map(Some)
}

controller_action!(set_sideboat_visible(visible: bool) => set_sideboat_visible(visible));

#[tauri::command]
fn unpair_device(controller: ControllerState<'_>, device_id: String) {
    controller.unpair_device(&device_id);
}

controller_action!(respond_to_pairing(accepted: bool) => respond_to_pairing(accepted));

controller_action!(dismiss_notification(index: usize) => dismiss_notification(index));

#[tauri::command]
fn clear_notifications(controller: ControllerState<'_>) {
    controller.clear_notifications();
}

#[tauri::command]
fn notification_icon(
    controller: ControllerState<'_>,
    icon_key: String,
) -> Result<Option<String>, String> {
    controller.notification_icon(&icon_key)
}

#[tauri::command]
fn sms_contact_photo(controller: ControllerState<'_>, thread_id: i64) -> Option<String> {
    controller.sms_contact_photo(thread_id)
}

controller_action!(select_sms_thread(thread_id: i64) => select_sms_thread(thread_id));
controller_action!(send_sms(thread_id: i64, body: String) => send_sms(thread_id, body));
controller_action!(clear_sms_cache() => clear_sms_cache());

#[tauri::command]
fn clear_clipboard(controller: ControllerState<'_>) {
    controller.clear_clipboard();
}

controller_action!(send_clipboard(hash: String) => send_clipboard(&hash));
controller_action!(copy_clipboard_local(hash: String) => copy_clipboard_local(&hash));

#[tauri::command]
fn clear_transfers(controller: ControllerState<'_>) {
    controller.clear_transfers();
}

#[tauri::command]
async fn import_dropped_files(
    controller: ControllerState<'_>,
    paths: Vec<String>,
) -> Result<DroppedFileImport, String> {
    let folder = controller.shared_folder_path();
    tauri::async_runtime::spawn_blocking(move || import_files_into_shared_folder(folder, paths))
        .await
        .map_err(|error| format!("Could not import dropped files: {error}"))
}

controller_action!(open_shared_folder() => open_shared_folder());
controller_action!(choose_shared_folder() => choose_shared_folder());
controller_action!(open_transfer(path: String) => open_transfer(&path));
controller_action!(create_command(name: String, command: String, description: String, detach: bool) => create_command(name, command, description, detach));
controller_action!(delete_command(id: String) => delete_command(&id));
controller_action!(update_command(id: String, name: String, command: String, description: String, detach: bool) => update_command(&id, name, command, description, detach));
controller_action!(run_command(id: String) => run_command(&id));
controller_action!(media_command(source: String, command: String) => media_command(&source, &command));

#[tauri::command]
fn clear_logs(controller: ControllerState<'_>) {
    controller.clear_logs();
}

controller_action!(setup_camera(retry: bool) => setup_camera(retry));

#[tauri::command]
fn set_camera_device(controller: ControllerState<'_>, device_id: Option<String>) {
    controller.set_camera_device(device_id);
}

controller_action!(set_camera_auto_load(enabled: bool) => set_camera_auto_load(enabled));
controller_action!(set_camera_stream_params(fps: u32, bitrate_kbps: u32) => set_camera_stream_params(fps, bitrate_kbps));
controller_action!(set_camera_zoom(value: f32) => set_camera_zoom(value));
controller_action!(set_camera_exposure(value: i32) => set_camera_exposure(value));
controller_action!(set_camera_torch(enabled: bool) => set_camera_torch(enabled));
controller_action!(switch_camera() => switch_camera());

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .setup(|app| {
            setup_tray(app)?;
            setup_window_icon(app)?;
            let controller = Arc::new(crate::ui_bridge::start_desktop_controller());
            let event_app = app.handle().clone();
            controller.clone().spawn_event_notifier(move || {
                let _ = event_app.emit("state-dirty", ());
            });
            app.manage(controller);
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main"
                && let tauri::WindowEvent::CloseRequested { api, .. } = event
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            desktop_state,
            set_streaming,
            select_output,
            create_virtual_display,
            destroy_virtual_display,
            screen_preview,
            set_sideboat_visible,
            unpair_device,
            respond_to_pairing,
            dismiss_notification,
            clear_notifications,
            notification_icon,
            sms_contact_photo,
            select_sms_thread,
            send_sms,
            clear_sms_cache,
            clear_clipboard,
            send_clipboard,
            copy_clipboard_local,
            clear_transfers,
            import_dropped_files,
            open_shared_folder,
            choose_shared_folder,
            open_transfer,
            create_command,
            delete_command,
            update_command,
            run_command,
            media_command,
            clear_logs,
            setup_camera,
            set_camera_device,
            set_camera_auto_load,
            set_camera_stream_params,
            set_camera_zoom,
            set_camera_exposure,
            set_camera_torch,
            switch_camera
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Anchor Tauri desktop");
}
