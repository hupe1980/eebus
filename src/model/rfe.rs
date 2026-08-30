//! Restricted Function Exchange: partial reads, partial writes and deletes.
//!
//! SPINE lets a peer exchange a *subset* of a function's data instead of the whole
//! thing (Protocol Specification §5.3.4). A command carries [`Filter`](crate::model::Filter)s
//! that say which entries are addressed (*selectors*), which elements of them
//! (*elements*), and whether this is a partial update or a delete (`cmdControl`).
//!
//! The rules are short but easy to get wrong, and the SPINE implementation guide §3.3
//! devotes a table to the one that bites hardest:
//!
//! | `scaledNumber` sent | Full exchange | Partial exchange |
//! |---|---|---|
//! | `number` and `scale` | both apply | update both |
//! | `number` only | **invalid** | update `number`, keep the stored `scale` |
//! | `scale` only | **invalid** | keep the stored `number`, update `scale` |
//! | neither | **invalid** | unchanged |
//!
//! In other words, a partial update merges *into the stored value*, element by element,
//! all the way down — it does not replace it. Getting that wrong turns a 4.2 kW limit
//! into 42 MW the first time a peer sends a bare `number`.
//!
//! ```
//! use eebus::model::rfe::{self, Identified};
//! use eebus::model::{LoadControlLimitData, LoadControlLimitId, LoadControlLimitListData,
//!                    ScaledNumber, Number};
//!
//! let mut stored = LoadControlLimitListData {
//!     load_control_limit_data: Some(vec![LoadControlLimitData {
//!         limit_id: Some(LoadControlLimitId(1)),
//!         is_limit_active: Some(true),
//!         value: Some(ScaledNumber::new(42, 2)),          // 4200 W
//!         ..Default::default()
//!     }]),
//! };
//!
//! // A partial update that carries only the mantissa.
//! let update = LoadControlLimitListData {
//!     load_control_limit_data: Some(vec![LoadControlLimitData {
//!         limit_id: Some(LoadControlLimitId(1)),
//!         value: Some(ScaledNumber { number: Some(Number(30)), scale: None }),
//!         ..Default::default()
//!     }]),
//! };
//!
//! rfe::apply_partial(&mut stored, update);
//!
//! let entry = &stored.load_control_limit_data.as_ref().unwrap()[0];
//! assert_eq!(entry.value.as_ref().unwrap().to_f64(), Some(3000.0), "the scale was kept");
//! assert_eq!(entry.is_limit_active, Some(true), "untouched elements survive");
//! ```

use alloc::vec::Vec;

use crate::codec::Merge;

/// A command carried a payload for a different function than the one stored.
///
/// SPINE addresses a function by name, so this is a routing error rather than a merge
/// that failed: the recipient answers with `errorNumber` 6, "command not supported".
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("expected a `{stored}` payload, received `{received}`")]
pub struct FunctionMismatch {
    /// The function the stored data belongs to.
    pub stored: &'static str,
    /// The function the received payload belongs to.
    pub received: &'static str,
}

/// A list entry that can be told apart from its siblings.
pub trait Identified {
    /// The Rust names of the elements that identify an entry, in schema order.
    const IDENTIFIER_FIELDS: &'static [&'static str];

    /// True when every identifying element is present.
    ///
    /// The use-case implementation guide §3.1 requires all primary and sub identifiers
    /// in every message, "regardless of being writeable or not", so an entry without
    /// them cannot be addressed and is a protocol error rather than a new entry.
    fn has_identity(&self) -> bool;

    /// True when `self` and `other` identify the same entry.
    fn same_entry(&self, other: &Self) -> bool;
}

/// A SPINE `*ListData` function: a list of identified entries.
pub trait ListData {
    /// The type of one entry.
    type Item;

    /// The stored entries, if any.
    fn entries(&self) -> Option<&[Self::Item]>;

    /// The stored entries, for modification.
    fn entries_mut(&mut self) -> &mut Option<Vec<Self::Item>>;
}

/// An *elements* filter: which elements of an entry a command addresses.
pub trait Elements {
    /// The data type this filter addresses.
    type Target;

    /// Removes the addressed elements from `target`.
    fn clear_from(&self, target: &mut Self::Target);

    /// Removes everything *except* the addressed elements, which is what a partial
    /// *read* returns.
    fn retain_in(&self, target: &mut Self::Target);
}

/// A *selectors* filter: which entries of a list a command addresses.
pub trait Selectors {
    /// The entry type this filter selects from.
    type Target;

    /// Elements of this filter that cannot be matched by direct comparison, such as the
    /// interval selectors.
    ///
    /// A command using one of them must be answered with SPINE `errorNumber` 8 rather
    /// than served incorrectly.
    const UNSUPPORTED_FIELDS: &'static [&'static str];

    /// True when `target` satisfies every element the filter sets.
    fn matches(&self, target: &Self::Target) -> bool;

    /// True when the filter constrains nothing, and therefore selects every entry.
    fn is_empty(&self) -> bool;
}

/// Applies a full (non-partial) update: the list is replaced.
pub fn apply_full<L: ListData>(stored: &mut L, mut update: L) {
    *stored.entries_mut() = update.entries_mut().take();
}

/// Applies a partial update, merging each entry into the stored entry it identifies.
///
/// Entries whose identifiers match nothing stored are appended, which is how a peer
/// adds a list entry without resending the list.
pub fn apply_partial<L>(stored: &mut L, mut update: L)
where
    L: ListData,
    L::Item: Identified + Merge,
{
    let Some(incoming) = update.entries_mut().take() else {
        return;
    };
    let entries = stored.entries_mut().get_or_insert_with(Vec::new);

    for item in incoming {
        match entries.iter_mut().find(|e| e.same_entry(&item)) {
            Some(existing) => existing.merge(item),
            None => entries.push(item),
        }
    }
}

/// Deletes whole entries: those the update identifies are removed from the list.
pub fn delete_entries<L>(stored: &mut L, update: &L)
where
    L: ListData,
    L::Item: Identified,
{
    let Some(targets) = update.entries() else {
        return;
    };
    if let Some(entries) = stored.entries_mut() {
        entries.retain(|existing| !targets.iter().any(|t| t.same_entry(existing)));
    }
}

/// Deletes elements *within* entries: the entries stay, the addressed elements go.
///
/// This is the shape LPC §3.4.1.4 uses to withdraw a limit's `endTime` while writing a
/// new value in the same command: deleting the whole entry would drop the limit.
pub fn delete_elements<L, E>(stored: &mut L, selector: impl Fn(&L::Item) -> bool, elements: &E)
where
    L: ListData,
    E: Elements<Target = L::Item>,
{
    if let Some(entries) = stored.entries_mut() {
        for entry in entries.iter_mut().filter(|e| selector(e)) {
            elements.clear_from(entry);
        }
    }
}

/// Selects the entries a partial read should answer with.
pub fn select<'a, L, S>(stored: &'a L, selectors: &S) -> Vec<&'a L::Item>
where
    L: ListData,
    S: Selectors<Target = L::Item>,
{
    stored
        .entries()
        .unwrap_or_default()
        .iter()
        .filter(|item| selectors.matches(item))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        LoadControlLimitData, LoadControlLimitDataElements, LoadControlLimitId,
        LoadControlLimitListData, LoadControlLimitListDataSelectors, Number, ScaledNumber,
        TimePeriod, TimePeriodElements,
    };
    use alloc::vec;

    fn limit(id: u32, watts: i64) -> LoadControlLimitData {
        LoadControlLimitData {
            limit_id: Some(LoadControlLimitId(id)),
            is_limit_changeable: Some(true),
            is_limit_active: Some(true),
            value: Some(ScaledNumber::new(watts, 0)),
            ..Default::default()
        }
    }

    fn stored() -> LoadControlLimitListData {
        LoadControlLimitListData {
            load_control_limit_data: Some(vec![limit(1, 4200), limit(2, 11_000)]),
        }
    }

    /// `TC_SPINE_RTS_005`: the implementation guide's merge table for `scaledNumber`.
    #[test]
    fn tc_spine_rts_005_scaled_number_merge_table() {
        let cases: [(Option<i64>, Option<i16>, f64); 4] = [
            (Some(30), Some(1), 300.0), // both present: both update
            (Some(30), None, 3000.0),   // number only: the stored scale (2) is kept
            (None, Some(1), 420.0),     // scale only: the stored number (42) is kept
            (None, None, 4200.0),       // neither: unchanged
        ];

        for (number, scale, expected) in cases {
            let mut list = LoadControlLimitListData {
                load_control_limit_data: Some(vec![LoadControlLimitData {
                    limit_id: Some(LoadControlLimitId(1)),
                    value: Some(ScaledNumber::new(42, 2)), // 4200 W
                    ..Default::default()
                }]),
            };
            let update = LoadControlLimitListData {
                load_control_limit_data: Some(vec![LoadControlLimitData {
                    limit_id: Some(LoadControlLimitId(1)),
                    value: Some(ScaledNumber {
                        number: number.map(Number),
                        scale: scale.map(crate::model::Scale),
                    }),
                    ..Default::default()
                }]),
            };

            apply_partial(&mut list, update);

            let value = list.load_control_limit_data.as_ref().unwrap()[0]
                .value
                .as_ref()
                .unwrap();
            assert_eq!(
                value.to_f64(),
                Some(expected),
                "number={number:?} scale={scale:?}"
            );
        }
    }

    #[test]
    fn a_partial_update_leaves_untouched_elements_alone() {
        let mut list = stored();
        apply_partial(
            &mut list,
            LoadControlLimitListData {
                load_control_limit_data: Some(vec![LoadControlLimitData {
                    limit_id: Some(LoadControlLimitId(1)),
                    value: Some(ScaledNumber::new(2_000, 0)),
                    ..Default::default()
                }]),
            },
        );

        let entries = list.load_control_limit_data.as_ref().unwrap();
        assert_eq!(entries.len(), 2, "other entries are untouched");
        assert_eq!(entries[0].value.as_ref().unwrap().to_f64(), Some(2_000.0));
        assert_eq!(entries[0].is_limit_active, Some(true), "kept");
        assert_eq!(entries[1].value.as_ref().unwrap().to_f64(), Some(11_000.0));
    }

    #[test]
    fn an_unknown_entry_is_appended() {
        let mut list = stored();
        apply_partial(
            &mut list,
            LoadControlLimitListData {
                load_control_limit_data: Some(vec![limit(7, 500)]),
            },
        );
        assert_eq!(list.load_control_limit_data.as_ref().unwrap().len(), 3);
    }

    #[test]
    fn a_full_update_replaces_the_list() {
        let mut list = stored();
        apply_full(
            &mut list,
            LoadControlLimitListData {
                load_control_limit_data: Some(vec![limit(9, 1)]),
            },
        );
        let entries = list.load_control_limit_data.as_ref().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].limit_id, Some(LoadControlLimitId(9)));
    }

    #[test]
    fn deleting_an_entry_removes_only_that_entry() {
        let mut list = stored();
        delete_entries(
            &mut list,
            &LoadControlLimitListData {
                load_control_limit_data: Some(vec![LoadControlLimitData {
                    limit_id: Some(LoadControlLimitId(1)),
                    ..Default::default()
                }]),
            },
        );
        let entries = list.load_control_limit_data.as_ref().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].limit_id, Some(LoadControlLimitId(2)));
    }

    /// LPC §3.4.1.4: withdraw a limit's `endTime` without disturbing anything else.
    #[test]
    fn deleting_a_nested_element_keeps_the_entry() {
        let mut list = LoadControlLimitListData {
            load_control_limit_data: Some(vec![LoadControlLimitData {
                limit_id: Some(LoadControlLimitId(1)),
                is_limit_active: Some(true),
                time_period: Some(TimePeriod {
                    start_time: Some("2026-08-30T10:00:00Z".into()),
                    end_time: Some("2026-08-30T11:00:00Z".into()),
                }),
                value: Some(ScaledNumber::new(4_200, 0)),
                ..Default::default()
            }]),
        };

        let elements = LoadControlLimitDataElements {
            time_period: Some(TimePeriodElements {
                end_time: Some(crate::codec::ElementTag),
                ..Default::default()
            }),
            ..Default::default()
        };
        delete_elements(
            &mut list,
            |e| e.limit_id == Some(LoadControlLimitId(1)),
            &elements,
        );

        let entry = &list.load_control_limit_data.as_ref().unwrap()[0];
        let period = entry.time_period.as_ref().expect("the period survives");
        assert!(period.end_time.is_none(), "endTime is gone");
        assert!(period.start_time.is_some(), "startTime is not");
        assert_eq!(entry.value.as_ref().unwrap().to_f64(), Some(4_200.0));
    }

    /// An empty nested filter removes the whole structure.
    #[test]
    fn an_empty_nested_filter_removes_the_structure() {
        let mut list = LoadControlLimitListData {
            load_control_limit_data: Some(vec![LoadControlLimitData {
                limit_id: Some(LoadControlLimitId(1)),
                time_period: Some(TimePeriod {
                    start_time: Some("2026-08-30T10:00:00Z".into()),
                    end_time: Some("2026-08-30T11:00:00Z".into()),
                }),
                ..Default::default()
            }]),
        };
        let elements = LoadControlLimitDataElements {
            time_period: Some(TimePeriodElements::default()),
            ..Default::default()
        };
        delete_elements(&mut list, |_| true, &elements);
        assert!(
            list.load_control_limit_data.as_ref().unwrap()[0]
                .time_period
                .is_none()
        );
    }

    #[test]
    fn selectors_pick_out_matching_entries() {
        let list = stored();
        let selector = LoadControlLimitListDataSelectors {
            limit_id: Some(LoadControlLimitId(2)),
        };
        let picked = select(&list, &selector);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].limit_id, Some(LoadControlLimitId(2)));

        let all = select(&list, &LoadControlLimitListDataSelectors::default());
        assert_eq!(all.len(), 2, "an empty selector selects everything");
    }

    #[test]
    fn identifiers_are_the_leading_id_elements() {
        assert_eq!(LoadControlLimitData::IDENTIFIER_FIELDS, ["limit_id"]);
        assert!(limit(1, 1).same_entry(&limit(1, 999)));
        assert!(!limit(1, 1).same_entry(&limit(2, 1)));
        assert!(
            !LoadControlLimitData::default().same_entry(&LoadControlLimitData::default()),
            "an entry without identifiers matches nothing"
        );
    }
}
