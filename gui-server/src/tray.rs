use libloading::Library;
use serde_json::{Value, json};
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::ptr::null_mut;
use std::sync::OnceLock;

use super::{
    cli_status, desktop_env, ensure_watcher, load_prefs, new_session, paths, read_json, save_prefs,
};

pub(super) type Object = *mut c_void;
type Callback = unsafe extern "C" fn(Object, Object);
type Destroy = unsafe extern "C" fn(Object, Object);

macro_rules! api {
    ($gtk:ident, $gobject:ident, $glib:ident, $gio:ident; $($library:ident $function:ident: $signature:ty;)*) => {
        pub(super) struct Api {
            _libraries: [Library; 4],
            $(pub(super) $function: $signature,)*
        }

        impl Api {
            pub(super) fn load() -> Result<Self, libloading::Error> {
                unsafe {
                    let $gtk = Library::new("libgtk-3.so.0")?;
                    let $gobject = Library::new("libgobject-2.0.so.0")?;
                    let $glib = Library::new("libglib-2.0.so.0")?;
                    let $gio = Library::new("libgio-2.0.so.0")?;
                    $(let $function = *$library.get::<$signature>(concat!(stringify!($function), "\0").as_bytes())?;)*
                    Ok(Self { _libraries: [$gtk, $gobject, $glib, $gio], $($function,)* })
                }
            }
        }
    };
}

api! {
    gtk, gobject, glib, gio;
    gtk gtk_application_new: unsafe extern "C" fn(*const c_char, c_int) -> Object;
    gtk gtk_application_window_new: unsafe extern "C" fn(Object) -> Object;
    gtk gtk_window_set_title: unsafe extern "C" fn(Object, *const c_char);
    gtk gtk_window_set_default_size: unsafe extern "C" fn(Object, c_int, c_int);
    gtk gtk_window_set_keep_above: unsafe extern "C" fn(Object, c_int);
    gtk gtk_window_set_type_hint: unsafe extern "C" fn(Object, c_int);
    gtk gtk_window_set_icon: unsafe extern "C" fn(Object, Object);
    gtk gtk_window_present: unsafe extern "C" fn(Object);
    gtk gtk_window_close: unsafe extern "C" fn(Object);
    gtk gtk_window_set_titlebar: unsafe extern "C" fn(Object, Object);
    gtk gtk_header_bar_new: unsafe extern "C" fn() -> Object;
    gtk gtk_header_bar_set_title: unsafe extern "C" fn(Object, *const c_char);
    gtk gtk_header_bar_set_show_close_button: unsafe extern "C" fn(Object, c_int);
    gtk gtk_container_add: unsafe extern "C" fn(Object, Object);
    gtk gtk_container_get_children: unsafe extern "C" fn(Object) -> *mut libc::c_void;
    gtk gtk_box_new: unsafe extern "C" fn(c_int, c_int) -> Object;
    gtk gtk_box_pack_start: unsafe extern "C" fn(Object, Object, c_int, c_int, c_uint);
    gtk gtk_label_new: unsafe extern "C" fn(*const c_char) -> Object;
    gtk gtk_label_set_text: unsafe extern "C" fn(Object, *const c_char);
    gtk gtk_label_set_markup: unsafe extern "C" fn(Object, *const c_char);
    gtk gtk_label_set_line_wrap: unsafe extern "C" fn(Object, c_int);
    gtk gtk_label_set_ellipsize: unsafe extern "C" fn(Object, c_int);
    gtk gtk_button_new_with_label: unsafe extern "C" fn(*const c_char) -> Object;
    gtk gtk_scrolled_window_new: unsafe extern "C" fn(Object, Object) -> Object;
    gtk gtk_scrolled_window_set_policy: unsafe extern "C" fn(Object, c_int, c_int);
    gtk gtk_list_box_new: unsafe extern "C" fn() -> Object;
    gtk gtk_list_box_set_selection_mode: unsafe extern "C" fn(Object, c_int);
    gtk gtk_list_box_row_new: unsafe extern "C" fn() -> Object;
    gtk gtk_list_box_row_set_activatable: unsafe extern "C" fn(Object, c_int);
    gtk gtk_widget_set_halign: unsafe extern "C" fn(Object, c_int);
    gtk gtk_widget_set_hexpand: unsafe extern "C" fn(Object, c_int);
    gtk gtk_widget_set_vexpand: unsafe extern "C" fn(Object, c_int);
    gtk gtk_widget_set_margin_start: unsafe extern "C" fn(Object, c_int);
    gtk gtk_widget_set_margin_end: unsafe extern "C" fn(Object, c_int);
    gtk gtk_widget_set_margin_top: unsafe extern "C" fn(Object, c_int);
    gtk gtk_widget_set_margin_bottom: unsafe extern "C" fn(Object, c_int);
    gtk gtk_widget_get_style_context: unsafe extern "C" fn(Object) -> Object;
    gtk gtk_style_context_add_class: unsafe extern "C" fn(Object, *const c_char);
    gtk gtk_style_context_add_provider_for_screen: unsafe extern "C" fn(Object, Object, c_uint);
    gtk gtk_style_context_remove_provider_for_screen: unsafe extern "C" fn(Object, Object);
    gtk gtk_css_provider_new: unsafe extern "C" fn() -> Object;
    gtk gtk_css_provider_load_from_data: unsafe extern "C" fn(Object, *const c_char, isize, Object) -> c_int;
    gtk gtk_widget_hide: unsafe extern "C" fn(Object);
    gtk gtk_widget_set_no_show_all: unsafe extern "C" fn(Object, c_int);
    gtk gtk_widget_add_events: unsafe extern "C" fn(Object, c_int);
    gtk gtk_menu_popup_at_pointer: unsafe extern "C" fn(Object, Object);
    gtk gtk_message_dialog_new: unsafe extern "C" fn(Object, c_int, c_int, c_int, *const c_char, ...) -> Object;
    gtk gtk_widget_get_window: unsafe extern "C" fn(Object) -> Object;
    gtk gtk_widget_get_display: unsafe extern "C" fn(Object) -> Object;
    gtk gdk_cursor_new_from_name: unsafe extern "C" fn(Object, *const c_char) -> Object;
    gtk gdk_window_set_cursor: unsafe extern "C" fn(Object, Object);
    gtk gdk_screen_get_default: unsafe extern "C" fn() -> Object;
    gtk gdk_event_get_button: unsafe extern "C" fn(Object, *mut c_uint) -> c_int;
    gio g_application_run: unsafe extern "C" fn(Object, c_int, Object) -> c_int;
    glib g_timeout_add: unsafe extern "C" fn(c_uint, unsafe extern "C" fn(Object) -> c_int, Object) -> c_uint;
    glib g_list_free: unsafe extern "C" fn(Object);
    gtk gtk_init_check: unsafe extern "C" fn(Object, Object) -> c_int;
    gtk gtk_main: unsafe extern "C" fn();
    gtk gtk_main_quit: unsafe extern "C" fn();
    gtk gtk_menu_new: unsafe extern "C" fn() -> Object;
    gtk gtk_menu_item_new_with_label: unsafe extern "C" fn(*const c_char) -> Object;
    gtk gtk_image_menu_item_new_with_label: unsafe extern "C" fn(*const c_char) -> Object;
    gtk gtk_image_menu_item_set_image: unsafe extern "C" fn(Object, Object);
    gtk gtk_image_menu_item_set_always_show_image: unsafe extern "C" fn(Object, c_int);
    gtk gtk_image_new_from_icon_name: unsafe extern "C" fn(*const c_char, c_int) -> Object;
    gtk gtk_image_new_from_pixbuf: unsafe extern "C" fn(Object) -> Object;
    gtk gdk_pixbuf_new_from_file_at_scale: unsafe extern "C" fn(*const c_char, c_int, c_int, c_int, Object) -> Object;
    gtk gtk_menu_shell_append: unsafe extern "C" fn(Object, Object);
    gtk gtk_menu_item_set_submenu: unsafe extern "C" fn(Object, Object);
    gtk gtk_separator_menu_item_new: unsafe extern "C" fn() -> Object;
    gtk gtk_radio_menu_item_new_with_label: unsafe extern "C" fn(Object, *const c_char) -> Object;
    gtk gtk_radio_menu_item_get_group: unsafe extern "C" fn(Object) -> Object;
    gtk gtk_check_menu_item_set_active: unsafe extern "C" fn(Object, c_int);
    gtk gtk_check_menu_item_get_active: unsafe extern "C" fn(Object) -> c_int;
    gtk gtk_widget_set_sensitive: unsafe extern "C" fn(Object, c_int);
    gtk gtk_widget_show_all: unsafe extern "C" fn(Object);
    gtk gtk_widget_destroy: unsafe extern "C" fn(Object);
    gtk gdk_display_get_default: unsafe extern "C" fn() -> Object;
    gtk gdk_display_get_app_launch_context: unsafe extern "C" fn(Object) -> Object;
    gio g_app_info_create_from_commandline: unsafe extern "C" fn(*const c_char, *const c_char, c_int, Object) -> Object;
    gio g_app_launch_context_get_startup_notify_id: unsafe extern "C" fn(Object, Object, Object) -> *mut c_char;
    glib g_free: unsafe extern "C" fn(Object);
    glib g_timeout_add_seconds: unsafe extern "C" fn(c_uint, unsafe extern "C" fn(Object) -> c_int, Object) -> c_uint;
    glib g_source_remove: unsafe extern "C" fn(c_uint) -> c_int;
    gobject g_object_ref_sink: unsafe extern "C" fn(Object) -> Object;
    gobject g_object_unref: unsafe extern "C" fn(Object);
    gobject g_signal_connect_data: unsafe extern "C" fn(Object, *const c_char, Callback, Object, Destroy, c_uint) -> libc::c_ulong;

}

macro_rules! indicator_api {
    ($($function:ident: $signature:ty;)*) => {
        struct IndicatorApi {
            _library: Library,
            $($function: $signature,)*
        }

        impl IndicatorApi {
            fn load() -> Result<Self, libloading::Error> {
                unsafe {
                    let library = Library::new("libayatana-appindicator3.so.1")?;
                    $(let $function = *library.get::<$signature>(concat!(stringify!($function), "\0").as_bytes())?;)*
                    Ok(Self { _library: library, $($function,)* })
                }
            }
        }
    };
}

indicator_api! {
    app_indicator_new: unsafe extern "C" fn(*const c_char, *const c_char, c_int) -> Object;
    app_indicator_set_menu: unsafe extern "C" fn(Object, Object);
    app_indicator_set_status: unsafe extern "C" fn(Object, c_int);
    app_indicator_set_label: unsafe extern "C" fn(Object, *const c_char, *const c_char);
    app_indicator_set_title: unsafe extern "C" fn(Object, *const c_char);
    app_indicator_set_secondary_activate_target: unsafe extern "C" fn(Object, Object);
}

static INDICATOR_API: OnceLock<IndicatorApi> = OnceLock::new();

pub(super) static API: OnceLock<Api> = OnceLock::new();

pub(super) fn text(value: &str) -> CString {
    CString::new(value.replace('\0', "")).expect("NUL removed")
}

pub(super) fn command(file: &Path) -> Command {
    let mut cmd = Command::new(file);
    cmd.current_dir(&paths().root)
        .env_clear()
        .envs(desktop_env())
        .env("DOCKER_CONTEXT", "default")
        .stdin(Stdio::null());
    cmd
}

pub(super) fn launch(mut cmd: Command, detach: bool) {
    if detach {
        new_session(&mut cmd);
    }
    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(error) => eprintln!("Docked Browser tray: {error}"),
    }
}

pub(super) fn open_web() {
    launch(command(&paths().root.join("bin/start-gui")), true);
    let port = std::env::var("DOCKED_BROWSER_PORT")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("COCKPIT_PORT").ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "8787".into());
    let url = format!("http://127.0.0.1:{port}/");
    let mut cmd = command(Path::new("xdg-open"));
    cmd.arg(&url);
    new_session(&mut cmd);
    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(_) => {
            let mut cmd = command(Path::new("gio"));
            cmd.args(["open", &url]);
            launch(cmd, true);
        }
    }
}

enum Action {
    Profile(&'static str, String),
    Picker,
    Web,
    Theme(&'static str),
    Build,
    Quit,
}

unsafe extern "C" fn destroy_action(data: Object, _: Object) {
    unsafe {
        drop(Box::from_raw(data.cast::<Action>()));
    }
}

pub(super) fn token(api: &Api) -> String {
    unsafe {
        let display = (api.gdk_display_get_default)();
        if display.is_null() {
            return String::new();
        }
        let context = (api.gdk_display_get_app_launch_context)(display);
        if context.is_null() {
            return String::new();
        }
        let app = (api.g_app_info_create_from_commandline)(
            c"docked-browser".as_ptr(),
            c"Docked Browser".as_ptr(),
            0,
            null_mut(),
        );
        let id = if app.is_null() {
            null_mut()
        } else {
            let id = (api.g_app_launch_context_get_startup_notify_id)(context, app, null_mut());
            (api.g_object_unref)(app);
            id
        };
        (api.g_object_unref)(context);
        if id.is_null() {
            return String::new();
        }
        let result = CStr::from_ptr(id).to_string_lossy().into_owned();
        (api.g_free)(id.cast());
        result
    }
}

unsafe extern "C" fn activate(widget: Object, data: Object) {
    let api = API.get().expect("tray initialized");
    match unsafe { &*data.cast::<Action>() } {
        Action::Profile(action, profile) => {
            let mut cmd = command(&paths().script);
            cmd.args([*action, profile.as_str()]);
            if *action == "activate" {
                let id = token(api);
                if !id.is_empty() {
                    cmd.env("DOCKED_ACTIVATION_TOKEN", id);
                }
            }
            launch(cmd, false);
        }
        Action::Picker => launch(command(&paths().root.join("bin/focus-modal")), true),
        Action::Web => open_web(),
        Action::Theme(theme) => {
            if unsafe { (api.gtk_check_menu_item_get_active)(widget) } != 0 {
                let updates = json!({"theme": theme});
                if let Err(error) = save_prefs(updates.as_object().expect("object")) {
                    eprintln!("Docked Browser tray: {error}");
                }
            }
        }
        Action::Build => {
            let mut cmd = command(&paths().script);
            cmd.arg("build");
            launch(cmd, false);
        }
        Action::Quit => unsafe {
            (api.gtk_main_quit)();
        },
    }
}

fn connect(api: &Api, item: Object, signal: &CStr, action: Action) {
    let data = Box::into_raw(Box::new(action)).cast();
    // GObject owns the action until the corresponding menu item is destroyed.
    unsafe {
        (api.g_signal_connect_data)(item, signal.as_ptr(), activate, data, destroy_action, 0);
    }
}

struct Tray {
    indicator: Object,
    menu: Object,
    snapshot: Value,
}

impl Tray {
    fn refresh(&mut self, api: &Api) {
        let indicator_api = INDICATOR_API.get().expect("AppIndicator loaded");
        let status = cli_status();
        let update =
            read_json(&paths().state_dir.join("chrome-update-check.json")).unwrap_or(Value::Null);
        let theme = load_prefs().remove("theme").unwrap_or(Value::Null);
        let playing: Vec<_> = status["profiles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| row["state"] == "running")
            .collect();
        let snapshot = json!([
            playing,
            update["update_available"],
            update["latest_version"],
            theme
        ]);
        if self.snapshot == snapshot {
            return;
        }
        self.snapshot = snapshot;
        let current = playing
            .first()
            .and_then(|row| row["profile"].as_str())
            .unwrap_or_default();
        let (menu, picker) = unsafe {
            let menu = (api.gtk_menu_new)();
            (api.g_object_ref_sink)(menu);
            let add = |menu, label: &str, action| {
                let item = (api.gtk_menu_item_new_with_label)(text(label).as_ptr());
                if let Some(action) = action {
                    connect(api, item, c"activate", action);
                } else {
                    (api.gtk_widget_set_sensitive)(item, 0);
                }
                (api.gtk_menu_shell_append)(menu, item);
                item
            };
            let separator =
                || (api.gtk_menu_shell_append)(menu, (api.gtk_separator_menu_item_new)());
            let heading = if current.is_empty() {
                "Nothing Playing".into()
            } else {
                format!("Now Playing — {current}")
            };
            add(menu, &heading, None);
            separator();
            if super::truthy(update.get("update_available")) {
                let latest = update["latest_version"].as_str().unwrap_or_default();
                add(
                    menu,
                    &format!("★ Install Chrome Update ({latest})…"),
                    Some(Action::Build),
                );
                separator();
            }
            let picker = add(menu, "Open Picker…", Some(Action::Picker));
            separator();
            if playing.is_empty() {
                add(menu, "No browsers running", None);
            }
            for row in playing {
                let profile = row["profile"].as_str().expect("profile");
                let label = if row["active"] == true {
                    format!("● {profile}")
                } else {
                    profile.into()
                };
                let item = (api.gtk_image_menu_item_new_with_label)(text(&label).as_ptr());
                let icon = row["icon"].as_str().unwrap_or_default();
                let pixbuf = if icon.is_empty() {
                    null_mut()
                } else {
                    (api.gdk_pixbuf_new_from_file_at_scale)(
                        text(icon).as_ptr(),
                        22,
                        22,
                        1,
                        null_mut(),
                    )
                };
                let image = if pixbuf.is_null() {
                    (api.gtk_image_new_from_icon_name)(c"web-browser".as_ptr(), 1)
                } else {
                    let image = (api.gtk_image_new_from_pixbuf)(pixbuf);
                    (api.g_object_unref)(pixbuf);
                    image
                };
                (api.gtk_image_menu_item_set_image)(item, image);
                (api.gtk_image_menu_item_set_always_show_image)(item, 1);
                let sub = (api.gtk_menu_new)();
                for (label, action) in [
                    ("Focus", "activate"),
                    ("Pause", "pause"),
                    ("Resume", "activate"),
                    ("Close", "stop"),
                ] {
                    add(sub, label, Some(Action::Profile(action, profile.into())));
                }
                (api.gtk_menu_item_set_submenu)(item, sub);
                (api.gtk_menu_shell_append)(menu, item);
            }
            separator();
            add(menu, "Open Web UI", Some(Action::Web));
            let appearance = (api.gtk_menu_item_new_with_label)(c"Appearance".as_ptr());
            let themes = (api.gtk_menu_new)();
            let mut group = null_mut();
            let mut items = Vec::new();
            for (label, value) in [
                (c"Light", "light"),
                (c"Dark", "dark"),
                (c"System", "system"),
            ] {
                let item = (api.gtk_radio_menu_item_new_with_label)(group, label.as_ptr());
                group = (api.gtk_radio_menu_item_get_group)(item);
                (api.gtk_menu_shell_append)(themes, item);
                if theme == value {
                    (api.gtk_check_menu_item_set_active)(item, 1);
                }
                items.push((item, value));
            }
            for (item, value) in items {
                connect(api, item, c"toggled", Action::Theme(value));
            }
            (api.gtk_menu_item_set_submenu)(appearance, themes);
            (api.gtk_menu_shell_append)(menu, appearance);
            add(menu, "Quit", Some(Action::Quit));
            (api.gtk_widget_show_all)(menu);
            (menu, picker)
        };
        unsafe {
            (indicator_api.app_indicator_set_menu)(self.indicator, menu);
            (indicator_api.app_indicator_set_secondary_activate_target)(self.indicator, picker);
            if !self.menu.is_null() {
                (api.gtk_widget_destroy)(self.menu);
                (api.g_object_unref)(self.menu);
            }
            self.menu = menu;
            let label: String = current.chars().take(16).collect();
            let title = if current.is_empty() {
                "Docked Browser".into()
            } else {
                format!("Docked Browser — {current}")
            };
            (indicator_api.app_indicator_set_label)(
                self.indicator,
                text(&label).as_ptr(),
                c"".as_ptr(),
            );
            (indicator_api.app_indicator_set_title)(self.indicator, text(&title).as_ptr());
        }
    }
}

unsafe extern "C" fn refresh(data: Object) -> c_int {
    unsafe { &mut *data.cast::<Tray>() }.refresh(API.get().expect("tray initialized"));
    1
}

pub(super) fn run() -> Result<(), Box<dyn std::error::Error>> {
    let api = Api::load()?;
    if unsafe { (api.gtk_init_check)(null_mut(), null_mut()) } == 0 {
        return Err("GTK could not open the display".into());
    }
    let indicator_api = IndicatorApi::load()?;
    let _ = INDICATOR_API.set(indicator_api);
    let indicator_api = INDICATOR_API.get().expect("AppIndicator loaded");
    let _ = API.set(api);
    let api = API.get().expect("tray initialized");
    let p = paths();
    let icon = p.state_dir.join("icons/docked-browser-tray.png");
    let icon = if icon.is_file() {
        icon
    } else {
        p.static_dir.join("brand-tray.png")
    };
    let icon = icon
        .to_str()
        .filter(|_| icon.is_file())
        .unwrap_or("web-browser");
    let indicator = unsafe {
        (indicator_api.app_indicator_new)(
            c"org.dockedbrowser.tray".as_ptr(),
            text(icon).as_ptr(),
            0,
        )
    };
    if indicator.is_null() {
        return Err("Could not create the tray indicator".into());
    }
    let mut tray = Tray {
        indicator,
        menu: null_mut(),
        snapshot: Value::Null,
    };
    tray.refresh(api);
    unsafe {
        (indicator_api.app_indicator_set_status)(indicator, 1);
    }

    let mut dock = command(&p.script);
    dock.arg("refresh-dock")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    launch(dock, true);
    let mut scheduler = command(&p.root.join("bin/docked-predict-sleep"));
    let parent = std::process::id() as libc::pid_t;
    unsafe {
        scheduler.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            Ok(())
        });
    }
    let mut scheduler = scheduler.spawn().ok();
    ensure_watcher();
    unsafe {
        let timer = (api.g_timeout_add_seconds)(4, refresh, (&mut tray as *mut Tray).cast());
        (api.gtk_main)();
        (api.g_source_remove)(timer);
        (indicator_api.app_indicator_set_status)(indicator, 0);
        (api.g_object_unref)(indicator);
        (api.gtk_widget_destroy)(tray.menu);
        (api.g_object_unref)(tray.menu);
    }
    if let Some(child) = &mut scheduler {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}
