//! Most-recently-run palette entries, keyed by stable entry key (never by
//! title, so renaming a label does not erase anyone's history).

use std::collections::VecDeque;

const CAPACITY: usize = 16;

#[derive(Debug, Clone, Default)]
pub(crate) struct Recents {
    keys: VecDeque<String>,
}

impl Recents {
    /// Record a run: the key moves to the front.
    pub(crate) fn touch(&mut self, key: &str) {
        if let Some(pos) = self.position(key) {
            self.keys.remove(pos);
        }
        self.keys.push_front(key.to_string());
        self.keys.truncate(CAPACITY);
    }

    /// 0 is the most recent.
    pub(crate) fn position(&self, key: &str) -> Option<usize> {
        self.keys.iter().position(|k| k == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_moves_to_front_and_bounds_the_list() {
        let mut r = Recents::default();
        r.touch("a");
        r.touch("b");
        r.touch("a");
        assert_eq!(r.position("a"), Some(0));
        assert_eq!(r.position("b"), Some(1));
        for i in 0..40 {
            r.touch(&format!("k{i}"));
        }
        assert_eq!(r.keys.len(), CAPACITY);
        assert_eq!(r.position("a"), None);
    }
}
