//! Docked Browser, native: the same dashboard as the web UI at
//! http://127.0.0.1:8787/, drawn by nanoui instead of a Chrome tab. It talks
//! to the same Python server (server.py) over its JSON API and polls on the
//! same schedule as the page: /api/status every 4 s, /api/stats every 10 s.
const std = @import("std");
const nanoui = @import("nanoui");
const ui = nanoui.ui;

const green = ui.Color.hex(0x22c55e);
const orange = ui.Color.hex(0xf59e0b);
const grey = ui.Color.hex(0x8b8b8b);
const pause_bg = ui.Color.hex(0x4a3a10);
const pause_fg = ui.Color.hex(0xf5c518);
const resume_bg = ui.Color.hex(0x163d24);
const resume_fg = ui.Color.hex(0x4ade80);
const close_bg = ui.Color.hex(0x4a1d1d);
const close_fg = ui.Color.hex(0xf87171);
const open_bg = ui.Color.hex(0x1a7cff);

const Parsed = std.json.Parsed(std.json.Value);

pub const Row = struct {
    name: []const u8,
    running: bool = false,
    paused: bool = false,
    auto: bool = false,
    has_icon: bool = false,
    fn live(r: Row) bool {
        return r.running or r.paused;
    }
};

pub const State = struct {
    gpa: std.mem.Allocator,
    base: []const u8 = "http://127.0.0.1:8787",
    window: ?*nanoui.Window = null,
    status: ?Parsed = null,
    stats: ?Parsed = null,
    icons: std.StringHashMapUnmanaged([]u8) = .empty,
    icon_inflight: std.StringHashMapUnmanaged(void) = .empty,
    name: ui.TextBuffer = .{},
    busy: bool = false,
    msg_buf: [160]u8 = undefined,
    msg: []const u8 = "",
    err: bool = false,
    /// Set by tests: no network, no timers.
    offline: bool = false,
    frames: u64 = 0,
    log_frames: bool = false,

    pub fn start(s: *State) void {
        if (s.offline) return;
        s.fetchStatus();
        s.fetchStats();
        nanoui.App.after(4000, s, statusTick);
        nanoui.App.after(10000, s, statsTick);
    }

    pub fn deinit(s: *State) void {
        if (s.status) |*p| p.deinit();
        if (s.stats) |*p| p.deinit();
        var it = s.icons.iterator();
        while (it.next()) |e| {
            s.gpa.free(e.key_ptr.*);
            s.gpa.free(e.value_ptr.*);
        }
        s.icons.deinit(s.gpa);
        var it2 = s.icon_inflight.keyIterator();
        while (it2.next()) |k| s.gpa.free(k.*);
        s.icon_inflight.deinit(s.gpa);
        s.name.deinit();
    }

    fn redraw(s: *State) void {
        if (s.window) |w| w.redraw();
    }

    fn setMsg(s: *State, err: bool, comptime fmt: []const u8, args: anytype) void {
        s.msg = std.fmt.bufPrint(&s.msg_buf, fmt, args) catch s.msg_buf[0..0];
        s.err = err;
    }

    // ---- networking -------------------------------------------------------

    fn url(s: *State, buf: []u8, path: []const u8) []const u8 {
        return std.fmt.bufPrint(buf, "{s}{s}", .{ s.base, path }) catch "";
    }

    fn statusTick(ctx: ?*anyopaque) callconv(.c) void {
        const s: *State = @ptrCast(@alignCast(ctx));
        if (!s.busy) s.fetchStatus();
        nanoui.App.after(4000, s, statusTick);
    }

    fn statsTick(ctx: ?*anyopaque) callconv(.c) void {
        const s: *State = @ptrCast(@alignCast(ctx));
        s.fetchStats();
        nanoui.App.after(10000, s, statsTick);
    }

    fn fetchStatus(s: *State) void {
        var b: [256]u8 = undefined;
        nanoui.http.fetch(s.gpa, .{ .url = s.url(&b, "/api/status"), .timeout = 20 }, s, gotStatus);
    }

    fn fetchStats(s: *State) void {
        var b: [256]u8 = undefined;
        nanoui.http.fetch(s.gpa, .{ .url = s.url(&b, "/api/stats"), .timeout = 20 }, s, gotStats);
    }

    fn gotStatus(s: *State, r: nanoui.http.Response) void {
        if (r.status != 200) {
            s.setMsg(true, "Can't reach the server: {s}", .{if (r.err.len > 0) r.err else "bad answer"});
            return s.redraw();
        }
        s.takeStatus(r.body);
        s.redraw();
    }

    fn gotStats(s: *State, r: nanoui.http.Response) void {
        if (r.status != 200) return;
        const p = std.json.parseFromSlice(std.json.Value, s.gpa, r.body, .{}) catch return;
        if (s.stats) |*old| old.deinit();
        s.stats = p;
        s.redraw();
    }

    /// Takes a /api/status (or /api/action) answer in, and fetches icons
    /// it names that aren't loaded yet.
    pub fn takeStatus(s: *State, body: []const u8) void {
        const p = std.json.parseFromSlice(std.json.Value, s.gpa, body, .{}) catch return;
        if (p.value != .object or p.value.object.get("containers") == null) {
            var q = p;
            q.deinit();
            return;
        }
        if (s.status) |*old| old.deinit();
        s.status = p;
        if (s.offline) return;
        const icons = p.value.object.get("icons") orelse return;
        if (icons != .object) return;
        var it = icons.object.iterator();
        while (it.next()) |e| {
            if (e.value_ptr.* != .bool or !e.value_ptr.bool) continue;
            const name = e.key_ptr.*;
            if (s.icons.contains(name) or s.icon_inflight.contains(name)) continue;
            const key = s.gpa.dupe(u8, name) catch continue;
            s.icon_inflight.put(s.gpa, key, {}) catch {
                s.gpa.free(key);
                continue;
            };
            const job = s.gpa.create(IconJob) catch continue;
            job.* = .{ .s = s, .name = key };
            var b: [256]u8 = undefined;
            const path = std.fmt.bufPrint(&b, "{s}/api/icon/{s}", .{ s.base, name }) catch continue;
            nanoui.http.fetch(s.gpa, .{ .url = path, .timeout = 20 }, job, IconJob.done);
        }
    }

    const IconJob = struct {
        s: *State,
        name: []const u8,
        fn done(j: *IconJob, r: nanoui.http.Response) void {
            const s = j.s;
            defer s.gpa.destroy(j);
            if (r.status == 200 and r.body.len > 0) {
                if (s.gpa.dupe(u8, r.body)) |bytes| {
                    if (s.gpa.dupe(u8, j.name)) |k| {
                        s.icons.put(s.gpa, k, bytes) catch {
                            s.gpa.free(k);
                            s.gpa.free(bytes);
                        };
                    } else |_| s.gpa.free(bytes);
                } else |_| {}
            }
            s.redraw();
        }
    };

    fn act(s: *State, action: []const u8, profile: []const u8, label: []const u8) void {
        if (s.busy or s.offline) return;
        s.busy = true;
        s.setMsg(false, "{s} \u{201c}{s}\u{201d}\u{2026}", .{ label, profile });
        var body: [256]u8 = undefined;
        const json = if (std.mem.eql(u8, action, "build"))
            std.fmt.bufPrint(&body, "{{\"action\":\"build\"}}", .{}) catch return
        else
            std.fmt.bufPrint(&body, "{{\"action\":\"{s}\",\"profile\":{f}}}", .{ action, std.json.fmt(profile, .{}) }) catch return;
        var b: [256]u8 = undefined;
        nanoui.http.fetch(s.gpa, .{
            .url = s.url(&b, "/api/action"),
            .method = "POST",
            .headers = &.{.{ .name = "Content-Type", .value = "application/json" }},
            .body = json,
            .timeout = 600,
        }, s, acted);
        s.redraw();
    }

    fn acted(s: *State, r: nanoui.http.Response) void {
        s.busy = false;
        if (r.status == 0) {
            s.setMsg(true, "Request failed: {s}", .{r.err});
        } else {
            s.takeStatus(r.body);
            if (r.status == 200) s.setMsg(false, "Done", .{}) else s.setMsg(true, "Something went wrong (HTTP {d})", .{r.status});
            s.fetchStats();
        }
        s.redraw();
    }

    // ---- data ------------------------------------------------------------

    fn obj(v: std.json.Value, k: []const u8) ?std.json.Value {
        if (v != .object) return null;
        return v.object.get(k);
    }

    fn flag(v: std.json.Value, k: []const u8) bool {
        const x = obj(v, k) orelse return false;
        return x == .bool and x.bool;
    }

    /// Every browser: the saved profiles, with their containers' state.
    pub fn rows(s: *State, out: []Row) []Row {
        const st = s.status orelse return out[0..0];
        var n: usize = 0;
        const icons = obj(st.value, "icons");
        if (obj(st.value, "profiles")) |ps| if (ps == .array) for (ps.array.items) |p| {
            if (p != .string or n == out.len) continue;
            out[n] = .{ .name = p.string };
            if (icons) |ic| out[n].has_icon = flag(ic, p.string);
            n += 1;
        };
        if (obj(st.value, "containers")) |cs| if (cs == .array) for (cs.array.items) |cv| {
            const pv = obj(cv, "profile") orelse continue;
            if (pv != .string) continue;
            var i: usize = 0;
            while (i < n and !std.mem.eql(u8, out[i].name, pv.string)) i += 1;
            if (i == n) {
                if (n == out.len) continue;
                out[n] = .{ .name = pv.string };
                n += 1;
            }
            out[i].running = flag(cv, "running");
            out[i].paused = flag(cv, "paused");
            out[i].auto = flag(cv, "auto_paused");
        };
        std.mem.sort(Row, out[0..n], {}, lessRow);
        return out[0..n];
    }

    fn lessRow(_: void, a: Row, b: Row) bool {
        if (a.live() != b.live()) return a.live();
        if (a.live() and a.running != b.running) return a.running;
        return std.ascii.lessThanIgnoreCase(a.name, b.name);
    }

    fn statFor(s: *State, profile: []const u8) ?std.json.Value {
        const st = s.stats orelse return null;
        const cs = obj(st.value, "containers") orelse return null;
        if (cs != .array) return null;
        for (cs.array.items) |cv| {
            const pv = obj(cv, "profile") orelse continue;
            if (pv == .string and std.mem.eql(u8, pv.string, profile)) return cv;
        }
        return null;
    }

    // ---- view ------------------------------------------------------------

    pub fn view(s: *State, c: *ui.Context) void {
        const th = c.theme;
        s.frames += 1;
        if (s.log_frames) std.debug.print("frame {d} t={d:.2}\n", .{ s.frames, c.time });
        const page = ui.div(c).column().sizeFull().bg(th.background).scrollY().itemsCenter();
        defer page.end();
        const col = ui.div(c).column().wFull().maxW(640).px(16).py(20).gap(16);
        defer col.end();

        {
            const head = ui.div(c).row().wFull().itemsCenter().justifyBetween();
            defer head.end();
            ui.text(c, "One Chrome per name. Pause what you\u{2019}re not using.", .{ .size = 15, .color = th.muted });
        }

        s.updateBanner(c);

        var buf: [128]Row = undefined;
        const all = s.rows(&buf);

        const card = ui.div(c).column().wFull().rounded(16).bg(th.surface).border(th.border).borderW(1).clip();
        defer card.end();
        s.compose(c);

        var live_n: usize = 0;
        for (all) |r| if (r.live()) {
            live_n += 1;
        };
        if (live_n > 0) {
            section(c, "OPEN NOW");
            for (all) |r| if (r.live()) s.liveRow(c, r);
        }
        if (all.len > live_n) {
            section(c, "SAVED");
            for (all) |r| if (!r.live()) s.savedRow(c, r);
        }
        if (s.status == null) {
            const p = ui.div(c).px(18).py(16);
            defer p.end();
            ui.text(c, if (s.msg.len > 0) s.msg else "Loading\u{2026}", .{ .color = th.muted });
        }
    }

    fn updateBanner(s: *State, c: *ui.Context) void {
        const st = s.status orelse return;
        const u = obj(st.value, "chrome_update") orelse return;
        if (!flag(u, "update_available")) return;
        const latest = obj(u, "latest_version") orelse return;
        const inst = obj(u, "installed_version") orelse return;
        if (latest != .string or inst != .string) return;
        const blue = ui.Color.hex(0x5aa9ff);
        const b = ui.div(c).row().wFull().itemsCenter().gap(12).px(18).py(14).rounded(14)
            .bg(ui.Color.hex(0x0f2a47)).border(ui.Color.hex(0x1f4f80)).borderW(1);
        defer b.end();
        {
            const t = ui.div(c).column().grow().gap(4);
            defer t.end();
            ui.text(c, "Chrome update available", .{ .size = 15, .weight = .semibold, .color = blue });
            ui.text(c, c.print("Version {s} is ready to install (installed: {s})", .{ latest.string, inst.string }), .{ .size = 13, .color = blue, .wrap = true });
        }
        if (pill(c, "Install update", open_bg, ui.Color.white, 15, s.busy)) s.act("build", "", "Installing Chrome");
    }

    fn compose(s: *State, c: *ui.Context) void {
        const th = c.theme;
        const box = ui.div(c).column().wFull().gap(8).p(18).bg(th.surface_hover.alpha(0.4));
        defer box.end();
        {
            const r = ui.div(c).row().wFull().itemsCenter().gap(10);
            defer r.end();
            const res = ui.textField(c, &s.name, .{ .placeholder = "Browser name", .width = .fill, .size = 16 });
            const name = std.mem.trim(u8, s.name.text(), " ");
            const go = pill(c, "Open", open_bg, ui.Color.white, 16, s.busy or name.len == 0);
            if ((go or res.submitted) and name.len > 0) s.act("run", name, "Opening");
        }
        if (s.busy or s.msg.len > 0) ui.text(c, if (s.busy) s.msg else s.msg, .{ .size = 12, .color = if (s.err) close_fg else th.muted });
    }

    fn section(c: *ui.Context, title: []const u8) void {
        const th = c.theme;
        const d = ui.div(c).wFull().px(18).pt(12).pb(6).bg(th.background.alpha(0.35));
        defer d.end();
        ui.text(c, title, .{ .size = 11, .weight = .semibold, .color = th.muted });
    }

    fn avatar(s: *State, c: *ui.Context, r: Row) void {
        const th = c.theme;
        if (r.has_icon) if (s.icons.get(r.name)) |bytes| {
            if (ui.image(c, .{ .key = c.print("icon-{s}", .{r.name}), .bytes = bytes, .max_width = 44, .max_height = 44, .radius = 10, .scale = 1 })) return;
        };
        const a = ui.div(c).size(44, 44).rounded(10).bg(th.border).center();
        defer a.end();
        const n = @min(2, r.name.len);
        const ini = c.print("{s}", .{r.name[0..n]});
        const up: []u8 = @constCast(ini);
        for (up) |*ch| ch.* = std.ascii.toUpper(ch.*);
        ui.text(c, ini, .{ .size = 11, .weight = .semibold });
    }

    fn rowShell(c: *ui.Context) ui.Div {
        const th = c.theme;
        ui.divider(c);
        return ui.div(c).row().wFull().itemsCenter().gap(14).px(18).py(12).hover(th.surface_hover);
    }

    fn liveRow(s: *State, c: *ui.Context, r: Row) void {
        const th = c.theme;
        ui.keyInt(c, std.hash.Wyhash.hash(1, r.name));
        const d = rowShell(c);
        defer d.end();
        s.avatar(c, r);
        {
            const t = ui.div(c).column().grow().gap(4);
            defer t.end();
            ui.text(c, r.name, .{ .size = 16, .weight = .medium });
            const m = ui.div(c).row().itemsCenter().gap(6);
            defer m.end();
            const dot_c = if (r.running) green else orange;
            const dot = ui.div(c).size(8, 8).rounded(4).bg(dot_c);
            dot.end();
            ui.text(c, if (r.running) "Open" else if (r.auto) "Auto-sleep" else "Paused", .{ .size = 13, .color = th.muted });
            if (s.statFor(r.name)) |st| {
                const cpu = obj(st, "cpu_pct");
                const mem = obj(st, "mem_used");
                const cpu_f: f64 = if (cpu) |v| switch (v) {
                    .float => v.float,
                    .integer => @floatFromInt(v.integer),
                    else => 0,
                } else 0;
                const mem_s = if (mem) |v| (if (v == .string) v.string else "?") else "?";
                ui.text(c, c.print("CPU {d:.1}% \u{00b7} RAM {s}", .{ cpu_f, mem_s }), .{ .size = 11, .mono = true, .color = th.muted });
            }
        }
        if (r.running) {
            if (pill(c, "Pause", pause_bg, pause_fg, 13, s.busy)) s.act("pause", r.name, "Pausing");
        } else {
            if (pill(c, "Resume", resume_bg, resume_fg, 13, s.busy)) s.act("resume", r.name, "Resuming");
        }
        if (pill(c, "Close", close_bg, close_fg, 13, s.busy)) s.act("stop", r.name, "Closing");
    }

    fn savedRow(s: *State, c: *ui.Context, r: Row) void {
        const th = c.theme;
        ui.keyInt(c, std.hash.Wyhash.hash(2, r.name));
        const d = rowShell(c);
        defer d.end();
        s.avatar(c, r);
        {
            const t = ui.div(c).column().grow().gap(4);
            defer t.end();
            ui.text(c, r.name, .{ .size = 16, .weight = .medium });
            const m = ui.div(c).row().itemsCenter().gap(6);
            defer m.end();
            const dot = ui.div(c).size(8, 8).rounded(4).bg(grey);
            dot.end();
            ui.text(c, "Not open", .{ .size = 13, .color = th.muted });
        }
        if (pill(c, "Open", open_bg, ui.Color.white, 13, s.busy)) s.act("run", r.name, "Opening");
    }
};

/// A rounded, colored button; true when clicked.
fn pill(c: *ui.Context, title: []const u8, bg: ui.Color, fg: ui.Color, size: f32, disabled: bool) bool {
    ui.key(c, title);
    const d = ui.div(c).row().px(size).py(size * 0.5).roundedFull().bg(bg).opacity(if (disabled) 0.5 else 1).control().hover(bg.mix(ui.Color.white, 0.12));
    defer d.end();
    ui.text(c, title, .{ .size = size, .weight = .medium, .color = fg });
    return !disabled and d.clicked();
}
