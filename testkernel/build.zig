const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.resolveTargetQuery(.{
        .abi = .none,
        .cpu_arch = .aarch64,
        .cpu_model = .{ .explicit = &std.Target.aarch64.cpu.cortex_a72 },
        .cpu_features_add = std.Target.aarch64.featureSet(&.{.strict_align}),
        .os_tag = .freestanding,
    });

    const exe = b.addExecutable(.{
        .name = "testkernel",
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/main.zig"),
            .target = target,
            .optimize = .ReleaseSmall,
        }),
    });
    exe.setLinkerScript(b.path("src/link.ld"));
    b.installArtifact(exe);

    const img = exe.addObjCopy(.{ .basename = "testkernel.img", .format = .bin });
    b.getInstallStep().dependOn(&b.addInstallBinFile(img.getOutput(), "testkernel.img").step);
}
