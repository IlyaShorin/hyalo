//! Frecency: how often and how recently an item was picked for a query.
//!
//! Each pick adds 1 to a weight that halves every two weeks, so one number
//! carries both frequency and recency. A pick made for "code" also counts
//! while the user is still typing it ("c", "co", "cod"), scaled by how much
//! of it has been typed.
//!
//! The clock is a parameter: callers pass `now`, tests pass any time.
//! The data is plain maps, vectors and numbers, so it serializes as is.

use crate::item::ItemId;
use crate::matcher;
use std::collections::BTreeMap;

/// Unix time in whole seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(pub u64);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frecency {
    picks: BTreeMap<ItemId, Vec<Pick>>,
}

/// The picks of one item for one query.
#[derive(Debug, Clone, PartialEq)]
struct Pick {
    /// Folded, never empty.
    query: String,
    /// The decayed number of picks as of `at`.
    weight: f64,
    at: Timestamp,
}

/// Two weeks, in seconds.
const HALF_LIFE: f64 = 1_209_600.0;

impl Frecency {
    /// Notes that `id` was picked when the query was `query`. A pick with
    /// an empty query says nothing about any query and is not kept.
    pub fn record(&mut self, id: &ItemId, query: &str, now: Timestamp) {
        let query = folded(query);
        if query.is_empty() {
            return;
        }
        let picks = self.picks.entry(id.clone()).or_default();
        match picks.iter_mut().find(|pick| pick.query == query) {
            Some(pick) => {
                pick.weight = pick.weight_at(now) + 1.0;
                pick.at = now;
            }
            None => picks.push(Pick {
                query,
                weight: 1.0,
                at: now,
            }),
        }
    }

    /// How strongly `id` has been picked for `query` and for the queries it
    /// starts, as of `now`. 0 when never.
    pub fn score(&self, id: &ItemId, query: &str, now: Timestamp) -> f64 {
        let query = folded(query);
        let typed = query.chars().count() as f64;
        let picks = self.picks.get(id).into_iter().flatten();
        picks
            .filter(|pick| pick.query.starts_with(&query))
            .map(|pick| pick.weight_at(now) * typed / pick.query.chars().count() as f64)
            .sum()
    }
}

impl Pick {
    /// The weight decayed to `now`. A clock set back counts as no time.
    fn weight_at(&self, now: Timestamp) -> f64 {
        let age = now.0.saturating_sub(self.at.0) as f64;
        self.weight * 0.5_f64.powf(age / HALF_LIFE)
    }
}

fn folded(query: &str) -> String {
    query.chars().map(matcher::fold).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: Timestamp = Timestamp(1_700_000_000);

    fn days(count: u64) -> Timestamp {
        Timestamp(START.0 + count * 24 * 60 * 60)
    }

    fn code() -> ItemId {
        ItemId("vscode".to_string())
    }

    fn picked(picks: &[(&str, Timestamp)]) -> Frecency {
        let mut frecency = Frecency::default();
        for &(query, at) in picks {
            frecency.record(&code(), query, at);
        }
        frecency
    }

    #[test]
    fn an_item_never_picked_scores_nothing() {
        assert_eq!(Frecency::default().score(&code(), "code", START), 0.0);
    }

    #[test]
    fn each_pick_for_a_query_adds_one() {
        assert_eq!(
            picked(&[("code", START)]).score(&code(), "code", START),
            1.0
        );
        let twice = picked(&[("code", START), ("code", START)]);
        assert_eq!(twice.score(&code(), "code", START), 2.0);
    }

    #[test]
    fn a_pick_halves_every_two_weeks() {
        let frecency = picked(&[("code", START)]);
        assert_eq!(frecency.score(&code(), "code", days(14)), 0.5);
        assert_eq!(frecency.score(&code(), "code", days(28)), 0.25);
    }

    #[test]
    fn a_new_pick_adds_to_what_is_left_of_the_old_ones() {
        let frecency = picked(&[("code", START), ("code", days(14))]);
        assert_eq!(frecency.score(&code(), "code", days(14)), 1.5);
        assert_eq!(frecency.score(&code(), "code", days(28)), 0.75);
    }

    #[test]
    fn a_pick_counts_while_its_query_is_typed_by_the_share_typed() {
        let frecency = picked(&[("code", START)]);
        assert_eq!(frecency.score(&code(), "cod", START), 0.75);
        assert_eq!(frecency.score(&code(), "co", START), 0.5);
        assert_eq!(frecency.score(&code(), "c", START), 0.25);
    }

    #[test]
    fn picks_for_different_queries_add_up() {
        let frecency = picked(&[("co", START), ("code", START)]);
        assert_eq!(frecency.score(&code(), "co", START), 1.5);
    }

    #[test]
    fn a_pick_does_not_count_for_other_queries() {
        let frecency = picked(&[("code", START)]);
        assert_eq!(frecency.score(&code(), "cx", START), 0.0);
        assert_eq!(frecency.score(&code(), "coder", START), 0.0);
        assert_eq!(frecency.score(&code(), "ode", START), 0.0);
    }

    #[test]
    fn a_pick_counts_only_for_its_item() {
        let frecency = picked(&[("code", START)]);
        let other = ItemId("chrome".to_string());
        assert_eq!(frecency.score(&other, "code", START), 0.0);
    }

    #[test]
    fn queries_compare_in_any_case() {
        let frecency = picked(&[("Code", START), ("КОД", START)]);
        assert_eq!(frecency.score(&code(), "CODE", START), 1.0);
        assert_eq!(frecency.score(&code(), "код", START), 1.0);
    }

    #[test]
    fn a_clock_set_back_does_not_grow_a_pick() {
        let frecency = picked(&[("code", days(14))]);
        assert_eq!(frecency.score(&code(), "code", START), 1.0);
    }

    #[test]
    fn a_pick_with_an_empty_query_is_not_kept() {
        let frecency = picked(&[("", START)]);
        assert_eq!(frecency, Frecency::default());
    }
}
