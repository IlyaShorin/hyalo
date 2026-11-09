//! Frecency: how often and how recently an item was picked for a query.
//!
//! Each pick adds 1 to a weight that halves every two weeks, so one number
//! carries both frequency and recency. A pick made for "code" also counts
//! while the user is still typing it ("c", "co", "cod"), scaled by how much
//! of it has been typed.
//!
//! The clock is a parameter: callers pass `now`, tests pass any time.
//! The picks are saved as small versioned JSON; the launcher writes it to
//! disk after every pick.

use crate::item::ItemId;
use crate::matcher;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Unix time in whole seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub u64);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frecency {
    picks: BTreeMap<ItemId, Vec<Pick>>,
}

/// The picks of one item for one query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

    /// Whether `record` could have made this pick.
    fn is_sound(&self) -> bool {
        !self.query.is_empty() && self.query == folded(&self.query) && self.weight > 0.0
    }
}

/// The saved format. A change to its shape gets a new number.
const VERSION: u32 = 1;

/// What is saved: the picks, under the format's version.
#[derive(Serialize, Deserialize)]
struct Saved<Picks> {
    version: u32,
    picks: Picks,
}

/// Why saved picks could not be read.
#[derive(Debug)]
pub enum LoadError {
    /// Not JSON, or not the shape this version saves.
    Json(serde_json::Error),
    /// Saved by a version of hyalo with another format.
    Version(u32),
    /// A pick `record` never makes: an empty or unfolded query, or a
    /// weight that is not above zero. Only a hand edit leaves one.
    Pick { id: ItemId, query: String },
}

impl Frecency {
    /// The picks as JSON: one line, small enough to write on every pick.
    pub fn to_json(&self) -> String {
        let saved = Saved {
            version: VERSION,
            picks: &self.picks,
        };
        serde_json::to_string(&saved).expect("string keys and finite numbers always serialize")
    }

    /// Reads picks that `to_json` wrote. The version is checked first, so
    /// a newer format is reported as such rather than as broken JSON.
    pub fn from_json(json: &str) -> Result<Self, LoadError> {
        let saved: Saved<serde_json::Value> =
            serde_json::from_str(json).map_err(LoadError::Json)?;
        if saved.version != VERSION {
            return Err(LoadError::Version(saved.version));
        }
        let picks: BTreeMap<ItemId, Vec<Pick>> =
            serde_json::from_value(saved.picks).map_err(LoadError::Json)?;
        match unsound_pick(&picks) {
            Some((id, pick)) => Err(LoadError::Pick {
                id: id.clone(),
                query: pick.query.clone(),
            }),
            None => Ok(Self { picks }),
        }
    }
}

fn unsound_pick(picks: &BTreeMap<ItemId, Vec<Pick>>) -> Option<(&ItemId, &Pick)> {
    picks
        .iter()
        .flat_map(|(id, picks)| picks.iter().map(move |pick| (id, pick)))
        .find(|(_, pick)| !pick.is_sound())
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

    #[test]
    fn saves_as_small_versioned_json() {
        assert_eq!(
            picked(&[("code", START)]).to_json(),
            r#"{"version":1,"picks":{"vscode":[{"query":"code","weight":1.0,"at":1700000000}]}}"#
        );
    }

    #[test]
    fn what_it_saves_loads_back_the_same() {
        let frecency = picked(&[("code", START), ("co", days(3)), ("code", days(20))]);
        let loaded = Frecency::from_json(&frecency.to_json()).expect("load");
        assert_eq!(loaded, frecency);
    }

    fn saved_pick(query: &str, weight: &str) -> String {
        format!(
            r#"{{"version":1,"picks":{{"vscode":[{{"query":"{query}","weight":{weight},"at":1700000000}}]}}}}"#
        )
    }

    #[test]
    fn loads_a_pick_with_any_positive_weight() {
        let frecency = Frecency::from_json(&saved_pick("code", "0.25")).expect("load");
        assert_eq!(frecency.score(&code(), "code", START), 0.25);
    }

    #[test]
    fn refuses_text_that_is_not_its_json() {
        for text in [
            "not json",
            r#"{"version":1}"#,
            r#"{"version":1,"picks":[]}"#,
        ] {
            let loaded = Frecency::from_json(text);
            assert!(matches!(loaded, Err(LoadError::Json(_))), "{text}");
        }
    }

    #[test]
    fn refuses_another_version_whatever_its_picks_look_like() {
        let json = r#"{"version":2,"picks":"a shape version 1 cannot read"}"#;
        assert!(matches!(
            Frecency::from_json(json),
            Err(LoadError::Version(2))
        ));
    }

    #[test]
    fn refuses_a_pick_it_would_never_save() {
        let broken = [
            ("", "1.0"),
            ("Code", "1.0"),
            ("code", "0.0"),
            ("code", "-1.0"),
        ];
        for (query, weight) in broken {
            let loaded = Frecency::from_json(&saved_pick(query, weight));
            assert!(
                matches!(loaded, Err(LoadError::Pick { .. })),
                "{query:?} {weight}"
            );
        }
    }

    #[test]
    fn refuses_a_weight_too_large_for_a_number() {
        assert!(Frecency::from_json(&saved_pick("code", "1e999")).is_err());
    }
}
