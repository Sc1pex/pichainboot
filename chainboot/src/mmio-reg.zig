const std = @import("std");

pub const FieldAccess = enum {
    RW,
    RO,
    WO,

    pub fn isWrite(self: FieldAccess) bool {
        return self == FieldAccess.RW or self == FieldAccess.WO;
    }
    pub fn isRead(self: FieldAccess) bool {
        return self == FieldAccess.RW or self == FieldAccess.RO;
    }
};

pub fn Field(comptime _offset: u8, comptime _width: u8, comptime _access: FieldAccess) type {
    return struct {
        pub const offset = _offset;
        pub const width = _width;
        pub const access = _access;
    };
}

pub fn FieldWithValues(comptime _offset: u8, comptime _width: u8, comptime _access: FieldAccess, comptime _values: type) type {
    const info = @typeInfo(_values);
    if (info != .@"enum") {
        @compileError("values must be an enum");
    }

    const enum_bits = @bitSizeOf(info.@"enum".tag_type);
    if (enum_bits > _width) {
        @compileError("values enum is too big to fit in the field width");
    }

    return struct {
        pub const offset = _offset;
        pub const width = _width;
        pub const access = _access;
        pub const values = _values;
    };
}

/// A struct containing the 4 types required for using a register
const RegisterStructs = struct {
    /// A packed struct with the actual representation of the hw register
    hw_repr: type,
    /// Contains the fields that can be read out of the register
    read: type,
    /// Contains the fields that can be written in the register
    write: type,
    /// Contains the same fields as the write struct, but every field
    /// is nullabable to allow writing just some of the fields
    modify: type,
};

pub fn Register(comptime width: u8, comptime fields: type) RegisterStructs {
    const info = @typeInfo(fields);
    if (info != .@"struct") {
        @compileError("fields must be a struct");
    }
    if (info.@"struct".fields.len == 0) {
        @compileError("fields cannot be an empty struct");
    }

    var padded_fields: [32]struct {
        name: []const u8,
        width: u8,
        field_type: type,
    } = undefined;
    var num_padded_fields = 0;
    var num_read_fields = 0;
    var num_write_fields = 0;

    var last_end: ?u8 = null;
    inline for (info.@"struct".fields) |field| {
        const start = last_end orelse 0;
        const offset_diff = field.type.offset - start;

        if (start > field.type.offset) {
            @compileError("Overlapping or out of order fields in register definition");
        } else if (offset_diff > 0) {
            padded_fields[num_padded_fields] = .{
                .name = std.fmt.comptimePrint("_reserved{d}", .{num_padded_fields}),
                .width = offset_diff,
                .field_type = @Int(.unsigned, offset_diff),
            };
            num_padded_fields += 1;
        }

        last_end = field.type.offset + field.type.width;
        padded_fields[num_padded_fields] = .{
            .name = field.name,
            .width = field.type.width,
            .field_type = if (@hasDecl(field.type, "values")) field.type.values else @Int(.unsigned, field.type.width),
        };
        num_padded_fields += 1;

        if (field.type.access.isRead()) {
            num_read_fields += 1;
        }
        if (field.type.access.isWrite()) {
            num_write_fields += 1;
        }
    }
    if (last_end.? < width) {
        padded_fields[num_padded_fields] = .{
            .name = std.fmt.comptimePrint("_reserved{d}", .{num_padded_fields}),
            .width = width - last_end.?,
            .field_type = @Int(.unsigned, width - last_end.?),
        };
        num_padded_fields += 1;
    }

    const hw_struct_fields = padded_fields[0..num_padded_fields];
    var hw_field_names: [num_padded_fields][]const u8 = undefined;
    var hw_field_types: [num_padded_fields]type = undefined;
    var hw_field_attrs: [num_padded_fields]std.builtin.Type.StructField.Attributes = undefined;

    inline for (hw_struct_fields, &hw_field_names, &hw_field_types, &hw_field_attrs) |field, *n, *t, *a| {
        n.* = field.name;
        t.* = field.field_type;
        a.* = .{
            .default_value_ptr = null,
            .@"align" = null,
            .@"comptime" = false,
        };
    }

    var read_field_names: [num_read_fields][]const u8 = undefined;
    var read_field_types: [num_read_fields]type = undefined;
    var read_field_attrs: [num_read_fields]std.builtin.Type.StructField.Attributes = undefined;
    var i = 0;
    inline for (info.@"struct".fields) |field| {
        if (!field.type.access.isRead()) {
            continue;
        }

        read_field_names[i] = field.name;
        read_field_types[i] = if (@hasDecl(field.type, "values")) field.type.values else @Int(.unsigned, field.type.width);
        read_field_attrs[i] = .{
            .default_value_ptr = null,
            .@"align" = null,
            .@"comptime" = false,
        };
        i += 1;
    }

    var write_field_names: [num_write_fields][]const u8 = undefined;
    var write_field_types: [num_write_fields]type = undefined;
    var write_field_attrs: [num_write_fields]std.builtin.Type.StructField.Attributes = undefined;

    var modify_field_names: [num_write_fields][]const u8 = undefined;
    var modify_field_types: [num_write_fields]type = undefined;
    var modify_field_attrs: [num_write_fields]std.builtin.Type.StructField.Attributes = undefined;
    i = 0;
    inline for (info.@"struct".fields) |field| {
        if (!field.type.access.isWrite()) {
            continue;
        }

        write_field_names[i] = field.name;
        write_field_types[i] = if (@hasDecl(field.type, "values")) field.type.values else @Int(.unsigned, field.type.width);
        write_field_attrs[i] = .{
            .default_value_ptr = null,
            .@"align" = null,
            .@"comptime" = false,
        };

        const default_value: ?write_field_types[i] = null;
        modify_field_names[i] = field.name;
        modify_field_types[i] = ?write_field_types[i];
        modify_field_attrs[i] = .{
            .default_value_ptr = &default_value,
            .@"align" = null,
            .@"comptime" = false,
        };

        i += 1;
    }

    return .{
        .hw_repr = @Struct(.@"packed", @Int(.unsigned, width), &hw_field_names, &hw_field_types, &hw_field_attrs),
        .read = @Struct(.auto, null, &read_field_names, &read_field_types, &read_field_attrs),
        .write = @Struct(.auto, null, &write_field_names, &write_field_types, &write_field_attrs),
        .modify = @Struct(.auto, null, &modify_field_names, &modify_field_types, &modify_field_attrs),
    };
}

fn RegisterAccessor(comptime Types: RegisterStructs) type {
    return extern struct {
        const Self = @This();

        _hw_repr: Types.hw_repr,

        pub inline fn read(self: *volatile Self) Types.read {
            const hw = self._hw_repr;

            var result: Types.read = undefined;
            inline for (@typeInfo(Types.read).@"struct".fields) |field| {
                @field(result, field.name) = @field(hw, field.name);
            }

            return result;
        }

        pub inline fn write(self: *volatile Self, value: Types.write) void {
            var hw: Types.hw_repr = std.mem.zeroes(Types.hw_repr);
            inline for (@typeInfo(Types.write).@"struct".fields) |field| {
                @field(hw, field.name) = @field(value, field.name);
            }
            const raw: @Int(.unsigned, @bitSizeOf(Types.hw_repr)) = @bitCast(hw);
            const ptr: *volatile @TypeOf(raw) = @ptrCast(self);
            ptr.* = raw;
        }

        pub inline fn modify(self: *volatile Self, value: Types.modify) void {
            var hw: Types.hw_repr = self._hw_repr;
            inline for (@typeInfo(Types.modify).@"struct".fields) |field| {
                if (@field(value, field.name)) |v| {
                    @field(hw, field.name) = v;
                }
            }
            const raw: @Int(.unsigned, @bitSizeOf(Types.hw_repr)) = @bitCast(hw);
            const ptr: *volatile @TypeOf(raw) = @ptrCast(self);
            ptr.* = raw;
        }
    };
}

pub fn RegisterMap(comptime registers: []const struct { []const u8, usize, RegisterStructs }) type {
    const n_regs = registers.len;
    var map_field_names: [n_regs * 2][]const u8 = undefined;
    var map_field_types: [n_regs * 2]type = undefined;
    var map_field_attrs: [n_regs * 2]std.builtin.Type.StructField.Attributes = undefined;

    var last_end: ?usize = null;
    var i = 0;
    inline for (registers) |entry| {
        const name, const offset, const reg = entry;

        const start = last_end orelse 0;
        const padding = offset - start;
        if (start > offset) {
            @compileError("Overlapping or out of order registers in register map");
        } else if (padding > 0) {
            map_field_names[i] = std.fmt.comptimePrint("_padding{}", .{i});
            map_field_types[i] = [padding]u8;
            map_field_attrs[i] = .{
                .default_value_ptr = null,
                .@"align" = null,
                .@"comptime" = false,
            };
            i += 1;
        }

        last_end = offset + @sizeOf(reg.hw_repr);
        map_field_names[i] = name;
        map_field_types[i] = RegisterAccessor(reg);
        map_field_attrs[i] = .{
            .default_value_ptr = null,
            .@"align" = null,
            .@"comptime" = false,
        };
        i += 1;
    }

    return @Struct(.@"extern", null, map_field_names[0..i], map_field_types[0..i], map_field_attrs[0..i]);
}
