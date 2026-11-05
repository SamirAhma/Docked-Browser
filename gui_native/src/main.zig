//! Docked Browser (native): one window showing the dashboard in app.zig.
//! DOCKED_URL overrides the server (default http://127.0.0.1:8787).
const std = @import("std");
const nanoui = @import("nanoui");
const app_view = @import("app.zig");

pub fn main(init: std.process.Init) !void {
    const app = try nanoui.App.create(init, .{ .name = "Docked Browser" });
    defer app.destroy();
    var state: app_view.State = .{ .gpa = init.gpa };
    defer state.deinit();
    state.log_frames = init.environ_map.get("DOCKED_FRAMES") != null;
    if (init.environ_map.get("DOCKED_URL")) |u| state.base = u;
    state.window = try app.newWindow(.{
        .title = "Docked Browser",
        .width = 680,
        .height = 900,
        .theme = nanoui.ui.Theme.dark_theme,
        .content = .{ .view = .init(&state, app_view.State.view) },
    });
    state.start();
    try app.run();
}
