/// Frequency of the ARM generic timer in Hz
fn frequency() u64 {
    return asm volatile ("mrs %[ret], cntfrq_el0"
        : [ret] "=r" (-> u64),
    );
}

fn counter() u64 {
    // Make sure the counter is not read ahead of preceding instructions
    asm volatile ("isb" ::: .{ .memory = true });
    return asm volatile ("mrs %[ret], cntpct_el0"
        : [ret] "=r" (-> u64),
    );
}

/// Microseconds since the timer was started
pub fn nowUs() u64 {
    const freq = frequency();
    const ticks = counter();
    // Split the conversion so that `ticks * 1_000_000` cannot overflow
    return (ticks / freq) * 1_000_000 + (ticks % freq) * 1_000_000 / freq;
}

pub fn delayUs(us: u64) void {
    const end = nowUs() + us;
    while (nowUs() < end) {}
}
