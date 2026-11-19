//! Shared row navigation helpers.

pub(crate) fn rotate_selectable<F>(
    cur: usize,
    count: usize,
    forward: bool,
    mut is_selectable: F,
) -> usize
where
    F: FnMut(usize) -> bool,
{
    if count == 0 {
        return 0;
    }
    let cur = cur.min(count - 1);
    let mut next = if forward {
        (cur + 1) % count
    } else if cur == 0 {
        count - 1
    } else {
        cur - 1
    };

    for _ in 0..count {
        if is_selectable(next) {
            return next;
        }
        next = if forward {
            (next + 1) % count
        } else if next == 0 {
            count - 1
        } else {
            next - 1
        };
    }

    cur.min(count.saturating_sub(1))
}

pub(super) fn rotate(cur: usize, count: usize, forward: bool) -> usize {
    rotate_selectable(cur, count, forward, |_| true)
}
