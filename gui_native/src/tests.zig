//! The view, driven by ui.Tester with canned server answers: no network.
const std = @import("std");
const nanoui = @import("nanoui");
const ui = nanoui.ui;
const app_view = @import("app.zig");

test "rows: live first (running before paused), then saved, case-insensitive" {
    var s: app_view.State = .{ .gpa = std.testing.allocator, .offline = true };
    defer s.deinit();
    s.takeStatus(@embedFile("sample_status.json"));
    var buf: [128]app_view.Row = undefined;
    const rows = s.rows(&buf);
    try std.testing.expect(rows.len >= 5);
    var seen_saved = false;
    for (rows, 0..) |r, i| {
        if (!(r.running or r.paused)) seen_saved = true else try std.testing.expect(!seen_saved);
        if (i > 0 and (rows[i - 1].running or rows[i - 1].paused) == (r.running or r.paused) and rows[i - 1].running == r.running)
            try std.testing.expect(std.ascii.lessThanIgnoreCase(rows[i - 1].name, r.name));
    }
}

test "renders the dashboard from a status answer" {
    var s: app_view.State = .{ .gpa = std.testing.allocator, .offline = true };
    defer s.deinit();
    s.takeStatus(@embedFile("sample_status.json"));
    var t: ui.Tester = undefined;
    try t.init(std.testing.allocator, .init(&s, app_view.State.view), .{ .width = 680, .height = 900, .theme = ui.Theme.dark_theme });
    defer t.deinit();
    t.settle(0.2);
    try std.testing.expect(t.hasText("SAVED"));
    try std.testing.expect(t.hasText("Not open"));
}
