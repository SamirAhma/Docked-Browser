use serde_json::Value;
use std::cell::RefCell;
use std::ffi::{CStr, c_int, c_uint, c_void};
use std::ptr::null_mut;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use super::tray::{API, Api, Object, command, open_web, text, token};
use super::{
    RunError, cli_status, load_prefs, paths, read_json, resolve_theme, row_str, run_capture, truthy,
};

thread_local! {
    static WINDOW: RefCell<Option<Modal>> = const { RefCell::new(None) };
}

fn api() -> &'static Api {
    API.get().expect("GTK loaded")
}

fn style(widget: Object, name: &CStr) {
    let a = api();
    unsafe {
        (a.gtk_style_context_add_class)((a.gtk_widget_get_style_context)(widget), name.as_ptr());
    }
}

fn pack(container: Object, widget: Object, expand: bool) {
    unsafe {
        (api().gtk_box_pack_start)(container, widget, expand.into(), expand.into(), 0);
    }
}

fn margins(widget: Object, vertical: c_int, horizontal: c_int) {
    let a = api();
    unsafe {
        (a.gtk_widget_set_margin_start)(widget, horizontal);
        (a.gtk_widget_set_margin_end)(widget, horizontal);
        (a.gtk_widget_set_margin_top)(widget, vertical);
        (a.gtk_widget_set_margin_bottom)(widget, vertical);
    }
}

fn label(value: &str) -> Object {
    let a = api();
    unsafe {
        let widget = (a.gtk_label_new)(text(value).as_ptr());
        (a.gtk_widget_set_halign)(widget, 1);
        widget
    }
}

enum Action {
    Refresh,
    Close,
    Web,
    Build,
    Profile(&'static str, String),
}

unsafe extern "C" fn drop_data<T>(data: Object, _: Object) {
    unsafe {
        drop(Box::from_raw(data.cast::<T>()));
    }
}

fn connect(widget: Object, signal: &CStr, action: Action) {
    unsafe {
        (api().g_signal_connect_data)(
            widget,
            signal.as_ptr(),
            action_callback,
            Box::into_raw(Box::new(action)).cast(),
            drop_data::<Action>,
            0,
        );
    }
}

fn button(title: &str, action: Action) -> Object {
    let widget = unsafe { (api().gtk_button_new_with_label)(text(title).as_ptr()) };
    connect(widget, c"clicked", action);
    widget
}

unsafe extern "C" fn action_callback(_: Object, data: Object) {
    let action = unsafe { &*data.cast::<Action>() };
    match action {
        Action::Web => open_web(),
        Action::Close => {
            let window = WINDOW.with(|state| state.borrow().as_ref().map(|modal| modal.window));
            if let Some(window) = window {
                unsafe {
                    (api().gtk_window_close)(window);
                }
            }
        }
        _ => WINDOW.with_borrow_mut(|state| {
            if let Some(modal) = state {
                match action {
                    Action::Refresh => {
                        if modal.pending.is_none() {
                            modal.reload();
                        }
                    }
                    Action::Build => modal.start("build", None),
                    Action::Profile(action, profile) => modal.start(action, Some(profile)),
                    _ => {}
                }
            }
        }),
    }
}

unsafe extern "C" fn popup(_: Object, event: Object, data: Object) -> c_int {
    let a = api();
    let mut pressed: c_uint = 0;
    if unsafe { (a.gdk_event_get_button)(event, &mut pressed) } == 0 || pressed != 3 {
        return 0;
    }
    let profile = unsafe { &*data.cast::<String>() };
    unsafe {
        let menu = (a.gtk_menu_new)();
        (a.g_object_ref_sink)(menu);
        for (title, action) in [
            ("Focus", "activate"),
            ("Pause", "pause"),
            ("Resume", "activate"),
            ("Close", "stop"),
        ] {
            let item = (a.gtk_menu_item_new_with_label)(text(title).as_ptr());
            connect(item, c"activate", Action::Profile(action, profile.clone()));
            (a.gtk_menu_shell_append)(menu, item);
        }
        (a.g_signal_connect_data)(
            menu,
            c"selection-done".as_ptr(),
            destroy_popup,
            null_mut(),
            ignore_destroy,
            0,
        );
        (a.gtk_widget_show_all)(menu);
        (a.gtk_menu_popup_at_pointer)(menu, event);
    }
    1
}

unsafe extern "C" fn ignore_destroy(_: Object, _: Object) {}

unsafe extern "C" fn destroy_popup(menu: Object, _: Object) {
    let a = api();
    unsafe {
        (a.gtk_widget_destroy)(menu);
        (a.g_object_unref)(menu);
    }
}

#[repr(C)]
struct GList {
    data: Object,
    next: *mut GList,
    previous: *mut GList,
}

struct Completion {
    action: &'static str,
    success: bool,
    output: String,
}

struct Modal {
    window: Object,
    list: Object,
    update_box: Object,
    update_label: Object,
    update_button: Object,
    theme: String,
    css: Object,
    timer: c_uint,
    last_reload: Instant,
    profiles: Value,
    rows: Vec<(Object, String)>,
    pending: Option<Receiver<Completion>>,
}

impl Modal {
    fn new(app: Object) -> Self {
        let a = api();
        unsafe {
            let window = (a.gtk_application_window_new)(app);
            (a.gtk_window_set_title)(window, c"Docked Browser".as_ptr());
            (a.gtk_window_set_default_size)(window, 380, 440);
            (a.gtk_window_set_keep_above)(window, 1);
            (a.gtk_window_set_type_hint)(window, 1);
            let brand = paths().state_dir.join("icons/docked-browser-modal.png");
            let brand = if brand.is_file() {
                brand
            } else {
                paths().static_dir.join("brand-modal.png")
            };
            let pixbuf = (a.gdk_pixbuf_new_from_file_at_scale)(
                text(&brand.to_string_lossy()).as_ptr(),
                64,
                64,
                1,
                null_mut(),
            );
            if !pixbuf.is_null() {
                (a.gtk_window_set_icon)(window, pixbuf);
                (a.g_object_unref)(pixbuf);
            }

            let bar = (a.gtk_header_bar_new)();
            (a.gtk_header_bar_set_title)(bar, c"Docked Browser".as_ptr());
            (a.gtk_header_bar_set_show_close_button)(bar, 1);
            style(bar, c"titlebar");
            (a.gtk_window_set_titlebar)(window, bar);

            let outer = (a.gtk_box_new)(1, 10);
            margins(outer, 16, 16);
            (a.gtk_container_add)(window, outer);
            let header = (a.gtk_box_new)(0, 8);
            let title = label("Profiles");
            style(title, c"section-title");
            pack(header, title, true);
            pack(header, button("Refresh", Action::Refresh), false);
            pack(outer, header, false);

            let hint = label("Click to open · Right-click to Pause, Resume, or Close");
            (a.gtk_label_set_line_wrap)(hint, 1);
            style(hint, c"hint");
            pack(outer, hint, false);

            let update_box = (a.gtk_box_new)(0, 8);
            style(update_box, c"update-box");
            (a.gtk_widget_set_no_show_all)(update_box, 1);
            let update_label = label("");
            let update_button = button("Install Update", Action::Build);
            style(update_button, c"suggested-action");
            pack(update_box, update_label, true);
            pack(update_box, update_button, false);
            pack(outer, update_box, false);

            let scrolled = (a.gtk_scrolled_window_new)(null_mut(), null_mut());
            (a.gtk_scrolled_window_set_policy)(scrolled, 2, 1);
            (a.gtk_widget_set_vexpand)(scrolled, 1);
            let list = (a.gtk_list_box_new)();
            let activate_row = std::mem::transmute::<
                unsafe extern "C" fn(Object, Object, Object),
                unsafe extern "C" fn(Object, Object),
            >(on_row_activated);
            (a.g_signal_connect_data)(
                list,
                c"row-activated".as_ptr(),
                activate_row,
                null_mut(),
                ignore_destroy,
                0,
            );
            (a.gtk_list_box_set_selection_mode)(list, 0);
            (a.gtk_container_add)(scrolled, list);
            pack(outer, scrolled, true);

            let footer = (a.gtk_box_new)(0, 8);
            let ui = button("Open Web UI", Action::Web);
            style(ui, c"suggested-action");
            pack(footer, ui, false);
            pack(footer, (a.gtk_box_new)(0, 0), true);
            pack(footer, button("Done", Action::Close), false);
            pack(outer, footer, false);

            (a.g_signal_connect_data)(
                window,
                c"destroy".as_ptr(),
                on_destroy,
                null_mut(),
                ignore_destroy,
                0,
            );
            Self {
                window,
                list,
                update_box,
                update_label,
                update_button,
                theme: String::new(),
                css: null_mut(),
                timer: 0,
                last_reload: Instant::now(),
                profiles: Value::Null,
                rows: Vec::new(),
                pending: None,
            }
        }
    }

    fn update_theme(&mut self) {
        let prefs = load_prefs();
        let theme = resolve_theme(row_str(&prefs, "theme"));
        if theme == self.theme {
            return;
        }
        self.theme = theme;
        self.profiles = Value::Null;
        let a = api();
        unsafe {
            let screen = (a.gdk_screen_get_default)();
            if screen.is_null() {
                return;
            }
            if !self.css.is_null() {
                (a.gtk_style_context_remove_provider_for_screen)(screen, self.css);
                (a.g_object_unref)(self.css);
            }
            self.css = (a.gtk_css_provider_new)();
            let css = if self.theme == "dark" {
                include_str!("modal-dark.css")
            } else {
                include_str!("modal-light.css")
            };
            (a.gtk_css_provider_load_from_data)(
                self.css,
                css.as_ptr().cast(),
                css.len() as isize,
                null_mut(),
            );
            (a.gtk_style_context_add_provider_for_screen)(screen, self.css, 800);
        }
    }

    fn reload(&mut self) {
        self.last_reload = Instant::now();
        self.update_theme();
        let a = api();
        let update =
            read_json(&paths().state_dir.join("chrome-update-check.json")).unwrap_or(Value::Null);
        unsafe {
            if truthy(update.get("update_available")) {
                let latest = update["latest_version"].as_str().unwrap_or_default();
                (a.gtk_label_set_text)(
                    self.update_label,
                    text(&format!("Chrome {latest} update ready")).as_ptr(),
                );
                (a.gtk_widget_set_sensitive)(self.update_button, 1);
                (a.gtk_widget_show_all)(self.update_label);
                (a.gtk_widget_show_all)(self.update_button);
                (a.gtk_widget_set_no_show_all)(self.update_box, 0);
                (a.gtk_widget_show_all)(self.update_box);
            } else {
                (a.gtk_widget_set_no_show_all)(self.update_box, 1);
                (a.gtk_widget_hide)(self.update_box);
            }
        }
        let mut status = cli_status();
        let profiles = status["profiles"].take();
        if profiles == self.profiles {
            return;
        }
        self.profiles = profiles;
        self.rows.clear();
        unsafe {
            let children = (a.gtk_container_get_children)(self.list);
            let mut node = children.cast::<GList>();
            while !node.is_null() {
                (a.gtk_widget_destroy)((*node).data);
                node = (*node).next;
            }
            (a.g_list_free)(children);
            let rows = self
                .profiles
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default();
            if rows.is_empty() {
                let row = (a.gtk_list_box_row_new)();
                style(row, c"empty-row");
                (a.gtk_list_box_row_set_activatable)(row, 0);
                let content = (a.gtk_box_new)(1, 8);
                margins(content, 48, 8);
                (a.gtk_widget_set_halign)(content, 3);
                let title = label("No profiles");
                style(title, c"empty-title");
                pack(content, title, false);
                let body = label("Create one from the Web UI.");
                style(body, c"empty-body");
                pack(content, body, false);
                (a.gtk_container_add)(row, content);
                (a.gtk_container_add)(self.list, row);
            } else {
                for profile in rows {
                    let row = self.profile_row(profile);
                    self.rows
                        .push((row, profile["profile"].as_str().expect("profile").into()));
                    (a.gtk_container_add)(self.list, row);
                }
            }
            (a.gtk_widget_show_all)(self.list);
        }
    }

    fn profile_row(&self, data: &Value) -> Object {
        let name = data["profile"].as_str().expect("valid profile");
        let state = data["state"].as_str().unwrap_or("stopped");
        let active = data["active"] == true;
        let dark = self.theme == "dark";
        let (state, color) = match (state, dark) {
            ("running", true) => ("Open", "#30d158"),
            ("running", false) => ("Open", "#34c759"),
            ("paused", true) => ("Paused", "#ff9f0a"),
            ("paused", false) => ("Paused", "#ff9500"),
            (_, true) => ("Closed", "#636366"),
            (_, false) => ("Closed", "#c7c7cc"),
        };
        let a = api();
        unsafe {
            let row = (a.gtk_list_box_row_new)();
            let line = (a.gtk_box_new)(0, 12);
            margins(line, 10, 8);
            let icon = data["icon"].as_str().unwrap_or_default();
            let pixbuf = if icon.is_empty() {
                null_mut()
            } else {
                (a.gdk_pixbuf_new_from_file_at_scale)(text(icon).as_ptr(), 36, 36, 1, null_mut())
            };
            let image = if pixbuf.is_null() {
                (a.gtk_image_new_from_icon_name)(c"web-browser".as_ptr(), 5)
            } else {
                let image = (a.gtk_image_new_from_pixbuf)(pixbuf);
                (a.g_object_unref)(pixbuf);
                image
            };
            pack(line, image, false);
            let words = (a.gtk_box_new)(1, 2);
            let title = label(name);
            (a.gtk_label_set_ellipsize)(title, 3);
            style(title, c"profile-name");
            pack(words, title, false);
            let subtitle = if active {
                format!("{state} · Front")
            } else {
                state.to_owned()
            };
            let subtitle = label(&subtitle);
            style(subtitle, c"profile-state");
            pack(words, subtitle, false);
            (a.gtk_widget_set_hexpand)(words, 1);
            pack(line, words, true);
            let dot = label("");
            (a.gtk_label_set_markup)(
                dot,
                text(&format!(
                    "<span foreground=\"{color}\" size=\"x-large\">●</span>"
                ))
                .as_ptr(),
            );
            pack(line, dot, false);
            (a.gtk_container_add)(row, line);
            (a.gtk_widget_add_events)(row, 1 << 8);
            let event_callback: unsafe extern "C" fn(Object, Object) =
                std::mem::transmute(popup as unsafe extern "C" fn(Object, Object, Object) -> c_int);
            (a.g_signal_connect_data)(
                row,
                c"button-press-event".as_ptr(),
                event_callback,
                Box::into_raw(Box::new(name.to_owned())).cast(),
                drop_data::<String>,
                0,
            );
            row
        }
    }

    fn set_busy_cursor(&self, busy: bool) {
        let a = api();
        unsafe {
            let window = (a.gtk_widget_get_window)(self.window);
            if window.is_null() {
                return;
            }
            let cursor = if busy {
                let display = (a.gtk_widget_get_display)(self.window);
                (a.gdk_cursor_new_from_name)(display, c"wait".as_ptr())
            } else {
                null_mut()
            };
            (a.gdk_window_set_cursor)(window, cursor);
            if !cursor.is_null() {
                (a.g_object_unref)(cursor);
            }
        }
    }

    fn start(&mut self, action: &'static str, profile: Option<&str>) {
        if self.pending.is_some() {
            return;
        }
        let mut cmd = command(&paths().script);
        cmd.arg(action);
        if let Some(profile) = profile {
            cmd.arg(profile);
        }
        if action == "activate" {
            let token = token(api());
            if !token.is_empty() {
                cmd.env("DOCKED_ACTIVATION_TOKEN", token);
            }
        }
        if action == "build" {
            let a = api();
            unsafe {
                (a.gtk_widget_set_sensitive)(self.update_button, 0);
                (a.gtk_label_set_text)(self.update_label, c"Installing update…".as_ptr());
            }
        }
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        self.set_busy_cursor(true);
        std::thread::spawn(move || {
            let timeout = Duration::from_secs(if action == "build" { 900 } else { 120 });
            let (success, output) = match run_capture(cmd, timeout, None) {
                Ok(output) => (
                    output.code == Some(0),
                    format!("{}{}", output.stdout, output.stderr),
                ),
                Err(RunError::Timeout) => (false, "Action timed out".into()),
                Err(RunError::Os(error)) => (false, error),
            };
            let _ = tx.send(Completion {
                action,
                success,
                output,
            });
        });
    }
}

unsafe extern "C" fn close_dialog(dialog: Object, _: c_int, _: Object) {
    unsafe {
        (api().gtk_widget_destroy)(dialog);
    }
}

unsafe extern "C" fn tick(_: Object) -> c_int {
    let window = WINDOW.with_borrow_mut(|state| {
        let modal = state.as_mut()?;
        if let Some(pending) = &modal.pending {
            match pending.try_recv() {
                Ok(result) => {
                    modal.pending = None;
                    modal.set_busy_cursor(false);
                    if result.success && result.action == "activate" {
                        return Some(modal.window);
                    }
                    modal.reload();
                    if result.action == "build" || !result.success {
                        let output: String = result.output.trim().chars().take(500).collect();
                        let message = if result.success {
                            "Chrome image updated. Close and reopen a browser to use it."
                        } else if output.is_empty() {
                            "Action failed"
                        } else {
                            &output
                        };
                        unsafe {
                            let a = api();
                            let dialog = (a.gtk_message_dialog_new)(
                                modal.window,
                                3,
                                if result.success { 0 } else { 1 },
                                1,
                                c"%s".as_ptr(),
                                text(message).as_ptr(),
                            );
                            let cb = std::mem::transmute::<
                                unsafe extern "C" fn(Object, c_int, Object),
                                unsafe extern "C" fn(Object, Object),
                            >(close_dialog);
                            (a.g_signal_connect_data)(
                                dialog,
                                c"response".as_ptr(),
                                cb,
                                null_mut(),
                                ignore_destroy,
                                0,
                            );
                            (a.gtk_widget_show_all)(dialog);
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    modal.pending = None;
                    modal.set_busy_cursor(false);
                    modal.reload();
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        } else if modal.last_reload.elapsed() >= Duration::from_secs(3) {
            modal.reload();
        }
        None
    });
    if let Some(window) = window {
        unsafe {
            (api().gtk_window_close)(window);
        }
    }
    1
}

unsafe extern "C" fn on_row_activated(_: Object, row: Object, _: Object) {
    WINDOW.with_borrow_mut(|state| {
        if let Some(modal) = state {
            if let Some((_, profile)) = modal.rows.iter().find(|(widget, _)| *widget == row) {
                let profile = profile.clone();
                modal.start("activate", Some(&profile));
            }
        }
    });
}

unsafe extern "C" fn on_destroy(_: Object, _: Object) {
    WINDOW.with_borrow_mut(|state| {
        if let Some(modal) = state.take() {
            let a = api();
            unsafe {
                if modal.timer != 0 {
                    (a.g_source_remove)(modal.timer);
                }
                if !modal.css.is_null() {
                    let screen = (a.gdk_screen_get_default)();
                    if !screen.is_null() {
                        (a.gtk_style_context_remove_provider_for_screen)(screen, modal.css);
                    }
                    (a.g_object_unref)(modal.css);
                }
            }
        }
    });
}

unsafe extern "C" fn on_activate(app: Object, _: Object) {
    WINDOW.with_borrow_mut(|state| {
        if let Some(modal) = state {
            unsafe {
                (api().gtk_window_present)(modal.window);
            }
            return;
        }
        let mut modal = Modal::new(app);
        modal.reload();
        unsafe {
            (api().gtk_widget_show_all)(modal.window);
            modal.timer = (api().g_timeout_add)(200, tick, null_mut());
        }
        *state = Some(modal);
    });
}

pub(super) fn run() -> Result<(), Box<dyn std::error::Error>> {
    let a = Api::load()?;
    // This command is single-threaded until GTK initializes.
    unsafe {
        std::env::set_var("GTK_THEME", "Adwaita");
    }
    if unsafe { (a.gtk_init_check)(null_mut(), null_mut()) } == 0 {
        return Err("GTK could not open the display".into());
    }
    let _ = API.set(a);
    let a = api();
    unsafe {
        let app = (a.gtk_application_new)(c"org.dockedbrowser.focus".as_ptr(), 0);
        if app.is_null() {
            return Err("Cannot create focus picker application".into());
        }
        (a.g_signal_connect_data)(
            app,
            c"activate".as_ptr(),
            on_activate,
            null_mut(),
            ignore_destroy,
            0,
        );
        let argument = c"focus-modal".as_ptr().cast_mut();
        let mut arguments = [argument, null_mut()];
        let status = (a.g_application_run)(app, 1, arguments.as_mut_ptr().cast::<c_void>());
        (a.g_object_unref)(app);
        if status == 0 {
            Ok(())
        } else {
            Err(format!("GTK exited with status {status}").into())
        }
    }
}
