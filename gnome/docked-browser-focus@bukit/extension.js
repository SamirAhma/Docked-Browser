import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';
import Gio from 'gi://Gio';
import Shell from 'gi://Shell';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const IFACE = `
<node>
  <interface name="org.dockedbrowser.WindowFocus">
    <method name="Activate">
      <arg type="au" direction="in" name="pids"/>
      <arg type="b" direction="out" name="ok"/>
    </method>
    <method name="ActivateClass">
      <arg type="s" direction="in" name="class_name"/>
      <arg type="b" direction="out" name="ok"/>
    </method>
  </interface>
</node>`;

function blankTitle(title) {
    const text = (title || '').toLowerCase();
    return text.includes('new tab') || text === 'about:blank' || text === 'google chrome';
}

function windowClassNames(win) {
    const names = [];
    const push = (value) => {
        if (value)
            names.push(String(value).toLowerCase());
    };
    try { push(win.get_wm_class()); } catch (_e) {}
    try { push(win.get_wm_class_instance()); } catch (_e) {}
    try {
        if (typeof win.get_gtk_application_id === 'function')
            push(win.get_gtk_application_id());
    } catch (_e) {}
    try {
        const app = Shell.WindowTracker.get_default().get_window_app(win);
        if (app)
            push(app.get_id());
    } catch (_e) {}
    return names;
}

function windowHasClass(win, className) {
    const wanted = String(className || '').toLowerCase();
    if (!wanted)
        return false;
    for (const name of windowClassNames(win)) {
        const bare = name.endsWith('.desktop') ? name.slice(0, -8) : name;
        if (name === wanted || bare === wanted)
            return true;
    }
    return false;
}

function raiseMatches(matches) {
    if (matches.length === 0)
        return false;
    const pages = matches.filter(win => !blankTitle(win.get_title()));
    const chosen = (pages.length ? pages : matches).at(-1);
    Main.activateWindow(chosen);
    return true;
}

export default class DockedBrowserFocus extends Extension {
    enable() {
        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        this._dbus.export(Gio.DBus.session, '/org/dockedbrowser/WindowFocus');
    }

    disable() {
        if (this._dbus) {
            this._dbus.flush();
            this._dbus.unexport();
            this._dbus = null;
        }
    }

    Activate(pids) {
        const wanted = new Set(pids.map(pid => Number(pid)));
        const matches = [];
        for (const actor of global.get_window_actors()) {
            const win = actor.meta_window;
            if (!win)
                continue;
            if (!wanted.has(win.get_pid()))
                continue;
            matches.push(win);
        }
        return raiseMatches(matches);
    }

    ActivateClass(className) {
        const matches = [];
        for (const actor of global.get_window_actors()) {
            const win = actor.meta_window;
            if (!win || !windowHasClass(win, className))
                continue;
            matches.push(win);
        }
        return raiseMatches(matches);
    }
}
