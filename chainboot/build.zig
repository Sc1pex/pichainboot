const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.resolveTargetQuery(.{
        .abi = .none,
        .cpu_arch = .aarch64,
        .cpu_model = .{ .explicit = &std.Target.aarch64.cpu.cortex_a72 },
        .os_tag = .freestanding,
    });
    const optimize = b.option(std.builtin.OptimizeMode, "optimize", "Prioritize performance, safety, or binary size") orelse .ReleaseSmall;

    const exe = b.addExecutable(.{
        .name = "chainboot",
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/main.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });
    const zregs = b.dependency("zregs", .{ .target = target });
    exe.root_module.addImport("zregs", zregs.module("zregs"));
    exe.root_module.addAssemblyFile(b.path("src/boot/boot.S"));
    exe.setLinkerScript(b.path("src/boot/link.ld"));

    b.installArtifact(exe);

    const kernel_img = exe.addObjCopy(.{
        .basename = "kernel8.img",
        .format = .bin,
    });
    b.getInstallStep().dependOn(&b.addInstallBinFile(
        kernel_img.getOutput(),
        "kernel8.img",
    ).step);
}
