const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    const mod = b.createModule(.{
        .root_source_file = b.path("src/root.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
        .pic = true,
    });
    const lib = b.addLibrary(.{
        .name = "ustanzig",
        .linkage = .static,
        .root_module = mod,
    });
    lib.bundle_compiler_rt = true;
    b.installArtifact(lib);

    const tests = b.addTest(.{ .root_module = mod });
    const run = b.addRunArtifact(tests);
    b.step("test", "Run Zig tests").dependOn(&run.step);
}
