//! What the renderer keeps of the icons it uploaded: the most recently
//! drawn ones, a bounded number of them. Rows whose ids never come back,
//! such as a closed window's, so leave the GPU in time instead of piling
//! up for as long as hyalo runs.

/// Values by key, the most recently used first, `bound` of them at most.
pub struct Recent<K, V> {
    entries: Vec<(K, V)>,
    bound: usize,
}

impl<K: PartialEq + Clone, V: Clone> Recent<K, V> {
    pub fn new(bound: usize) -> Self {
        Self {
            entries: Vec::new(),
            bound,
        }
    }

    /// The value kept for `key`, else the one `make` makes, which is kept.
    /// Either way `key` is then the most recently used; beyond the bound,
    /// the least recently used goes. A value that cannot be made changes
    /// nothing.
    pub fn get_or_make<E>(&mut self, key: &K, make: impl FnOnce() -> Result<V, E>) -> Result<V, E> {
        let entry = match self.entries.iter().position(|(kept, _)| kept == key) {
            Some(at) => self.entries.remove(at),
            None => (key.clone(), make()?),
        };
        let value = entry.1.clone();
        self.entries.insert(0, entry);
        self.entries.truncate(self.bound);
        Ok(value)
    }

    /// How many values are kept.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Icons = Recent<&'static str, String>;

    /// Asks `recent` for each of `keys` in turn; the keys whose values had
    /// to be made.
    fn made(recent: &mut Icons, keys: &[&'static str]) -> Vec<&'static str> {
        let mut made = Vec::new();
        for &key in keys {
            let value = recent.get_or_make(&key, || {
                made.push(key);
                Ok::<_, ()>(format!("{key}'s icon"))
            });
            assert_eq!(value, Ok(format!("{key}'s icon")), "the value for {key}");
        }
        made
    }

    #[test]
    fn a_value_is_made_once_while_it_is_kept() {
        let mut recent = Icons::new(2);
        assert_eq!(made(&mut recent, &["a", "b", "a", "b", "b"]), ["a", "b"]);
        assert_eq!(recent.len(), 2);
    }

    #[test]
    fn beyond_the_bound_the_least_recently_used_goes() {
        let mut recent = Icons::new(2);
        assert_eq!(made(&mut recent, &["a", "b", "c"]), ["a", "b", "c"]);
        assert_eq!(recent.len(), 2);
        assert_eq!(made(&mut recent, &["c", "b"]), [] as [&str; 0], "kept");
        assert_eq!(made(&mut recent, &["a"]), ["a"], "gone, so made again");
    }

    #[test]
    fn a_value_asked_for_again_is_the_most_recently_used() {
        let mut recent = Icons::new(2);
        assert_eq!(made(&mut recent, &["a", "b", "a", "c"]), ["a", "b", "c"]);
        assert_eq!(made(&mut recent, &["a"]), [] as [&str; 0], "used after b");
        assert_eq!(made(&mut recent, &["b"]), ["b"], "the least recently used");
    }

    #[test]
    fn a_value_that_cannot_be_made_is_not_kept_and_pushes_nothing_out() {
        let mut recent = Icons::new(1);
        assert_eq!(made(&mut recent, &["a"]), ["a"]);
        let failed = recent.get_or_make(&"b", || Err("no icon"));
        assert_eq!(failed, Err("no icon"));
        assert_eq!(made(&mut recent, &["a"]), [] as [&str; 0], "still kept");
        assert_eq!(made(&mut recent, &["b"]), ["b"], "made this time");
    }
}
