const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.resolveTargetQuery(.{
        .abi = .none,
        .cpu_arch = .aarch64,
        .cpu_model = .{ .explicit = &std.Target.aarch64.cpu.cortex_a72 },
        // The MMU is off, so all memory is Device memory and unaligned accesses fault
        .cpu_features_add = std.Target.aarch64.featureSet(&.{.strict_align}),
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

    // The protocol code does not touch hardware, so its tests run on the host
    const test_step = b.step("test", "Run protocol tests on the host");
    for ([_][]const u8{ "src/protocol.zig", "src/session.zig" }) |path| {
        const tests = b.addTest(.{
            .root_module = b.createModule(.{
                .root_source_file = b.path(path),
                .target = b.graph.host,
            }),
        });
        test_step.dependOn(&b.addRunArtifact(tests).step);
    }

    const kernel_img = exe.addObjCopy(.{
        .basename = "kernel8.img",
        .format = .bin,
    });
    b.getInstallStep().dependOn(&b.addInstallBinFile(
        kernel_img.getOutput(),
        "kernel8.img",
    ).step);
}
