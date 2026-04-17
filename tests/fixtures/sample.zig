const std = @import("std");

/// A 2D point
const Point = struct {
    x: f32,
    y: f32,

    pub fn distance(self: Point, other: Point) f32 {
        const dx = self.x - other.x;
        const dy = self.y - other.y;
        return @sqrt(dx * dx + dy * dy);
    }
};

/// Color enumeration
const Color = enum {
    red,
    green,
    blue,
};

/// Add two integers
pub fn add(a: i32, b: i32) i32 {
    return a + b;
}

/// Process a list of items
pub fn process(allocator: std.mem.Allocator) !void {
    var list = std.ArrayList(u8).init(allocator);
    defer list.deinit();
    std.debug.print("processing\n", .{});
}
