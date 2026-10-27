//! Splitting image work over the CPU's threads.

/// Run `f(first_row, rows)` over `buf` cut into bands of whole `row_bytes` rows, one band per available thread.
pub fn rows(buf: &mut [u8], row_bytes: usize, f: impl Fn(usize, &mut [u8]) + Sync) {
    let n = buf.len() / row_bytes.max(1);
    let threads = std::thread::available_parallelism().map_or(1, |t| t.get()).min(n.max(1));
    if threads <= 1 {
        f(0, buf);
        return;
    }
    let per = n.div_ceil(threads);
    std::thread::scope(|s| {
        for (i, band) in buf.chunks_mut(per * row_bytes).enumerate() {
            let f = &f;
            s.spawn(move || f(i * per, band));
        }
    });
}
