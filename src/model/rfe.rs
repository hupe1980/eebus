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

    /// Copies the identifying elements back from `from`.
    ///
    /// Used after an `elements` filter has cut an entry down: the identifiers are how
    /// the peer knows which entry it is looking at, so they are restored whether or not
    /// the filter named them.
    fn restore_identity(&mut self, from: &Self);
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
    ///
    /// Elements listed in [`UNSUPPORTED_FIELDS`](Self::UNSUPPORTED_FIELDS) are not
    /// considered, so a filter that sets one must be refused before this is consulted —
    /// see [`uses_unsupported`](Self::uses_unsupported).
    fn matches(&self, target: &Self::Target) -> bool;

    /// True when the filter constrains nothing, and therefore selects every entry.
    fn is_empty(&self) -> bool;

    /// True when the filter sets an element that cannot be matched by comparison.
    ///
    /// The answer is SPINE `errorNumber` 8, "restricted function exchange combination
    /// not supported". Serving such a request as though the element were absent would
    /// return entries the peer explicitly excluded, which is worse than refusing.
    fn uses_unsupported(&self) -> bool;
}

/// Why a Restricted Function Exchange filter could not be applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RestrictError {
    /// The filter addresses a different function than the data it was applied to.
    #[error("the filter addresses a different function than the data")]
    Mismatch,
    /// The filter sets an element this implementation cannot match, such as an
    /// interval selector.
    #[error("the filter uses a selector that cannot be matched by comparison")]
    Unsupported,
}

impl RestrictError {
    /// The SPINE `errorNumber` this failure is reported with.
    ///
    /// Both are §5.2.5.2's number 8: a filter naming the wrong function and one naming
    /// an element that cannot be matched are the same thing to the peer — a restricted
    /// exchange combination this node does not support.
    pub fn error_number(self) -> crate::model::ErrorNumber {
        crate::model::ErrorNumber::RestrictedExchangeNotSupported
    }
}

/// Answers a partial read of a list: the entries the selectors pick, cut down to the
/// elements the filter keeps.
///
/// Identifiers survive an `elements` filter whether or not it names them, because
/// SPINE addresses an entry by them: a reply whose entries cannot be told apart is not
/// a smaller answer, it is an unusable one (implementation guide §3.1).
pub fn restrict_list<L, S, E>(
    stored: &L,
    selectors: Option<&S>,
    elements: Option<&E>,
) -> Result<L, RestrictError>
where
    L: ListData + Default,
    L::Item: Identified + Clone,
    S: Selectors<Target = L::Item>,
    E: Elements<Target = L::Item>,
{
    if let Some(selectors) = selectors
        && selectors.uses_unsupported()
    {
        return Err(RestrictError::Unsupported);
    }

    let mut kept: Vec<L::Item> = stored
        .entries()
        .unwrap_or_default()
        .iter()
        .filter(|item| selectors.is_none_or(|s| s.matches(item)))
        .cloned()
        .collect();

    if let Some(elements) = elements {
        for entry in &mut kept {
            let identity = entry.clone();
            elements.retain_in(entry);
            entry.restore_identity(&identity);
        }
    }

    let mut out = L::default();
    *out.entries_mut() = Some(kept);
    Ok(out)
}

/// [`restrict_list`] for a list whose entries have no elements filter of their own.
pub fn restrict_list_by_selectors<L, S>(stored: &L, selectors: &S) -> Result<L, RestrictError>
where
    L: ListData + Default,
    L::Item: Clone,
    S: Selectors<Target = L::Item>,
{
    if selectors.uses_unsupported() {
        return Err(RestrictError::Unsupported);
    }
    let kept: Vec<L::Item> = stored
        .entries()
        .unwrap_or_default()
        .iter()
        .filter(|item| selectors.matches(item))
        .cloned()
        .collect();
    let mut out = L::default();
    *out.entries_mut() = Some(kept);
    Ok(out)
}

/// [`restrict_list`] for a list the schemas give no selectors filter.
pub fn restrict_list_by_elements<L, E>(stored: &L, elements: &E) -> L
where
    L: ListData + Default,
    L::Item: Identified + Clone,
    E: Elements<Target = L::Item>,
{
    let mut kept: Vec<L::Item> = stored.entries().unwrap_or_default().to_vec();
    for entry in &mut kept {
        let identity = entry.clone();
        elements.retain_in(entry);
        entry.restore_identity(&identity);
    }
    let mut out = L::default();
    *out.entries_mut() = Some(kept);
    out
}

/// Answers a partial read of a function that is not a list.
pub fn restrict_plain<T, E>(stored: &T, elements: &E) -> T
where
    T: Clone,
    E: Elements<Target = T>,
{
    let mut out = stored.clone();
    elements.retain_in(&mut out);
    out
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

/// Which entries a delete command addresses when it carries no selectors filter: the
/// ones its payload identifies, or — when the payload names none — all of them.
pub fn addresses_named<I: Identified>(named: &[I]) -> impl Fn(&I) -> bool + '_ {
    move |item| named.is_empty() || named.iter().any(|target| target.same_entry(item))
}

/// Which entries a selectors filter addresses.
pub fn addresses_selected<'a, I, S>(selectors: &'a S) -> impl Fn(&I) -> bool + 'a
where
    S: Selectors<Target = I>,
{
    move |item| selectors.matches(item)
}

/// Removes the entries `addressed` picks.
pub fn delete_addressed<L>(stored: &mut L, addressed: impl Fn(&L::Item) -> bool)
where
    L: ListData,
{
    if let Some(entries) = stored.entries_mut() {
        entries.retain(|entry| !addressed(entry));
    }
}

/// Deletes elements *within* the entries `addressed` picks: the entries stay, the
/// elements the filter names go.
///
/// This is the shape LPC UC TS §3.4.1.4 uses to withdraw a limit's `endTime` while
/// writing a new value in the same command — deleting the whole entry would drop the
/// limit, which is a curtailment lifted rather than one shortened.
///
/// Identifiers survive whether or not the filter names them, for the same reason they
/// survive a partial read: an entry that can no longer be told from its siblings is not
/// a smaller entry, it is a lost one (use-case implementation guide §3.1).
pub fn clear_addressed<L, E>(stored: &mut L, addressed: impl Fn(&L::Item) -> bool, elements: &E)
where
    L: ListData,
    L::Item: Identified + Clone,
    E: Elements<Target = L::Item>,
{
    if let Some(entries) = stored.entries_mut() {
        for entry in entries.iter_mut().filter(|e| addressed(e)) {
            let identity = entry.clone();
            elements.clear_from(entry);
            entry.restore_identity(&identity);
        }
    }
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
        clear_addressed(
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
        clear_addressed(&mut list, |_| true, &elements);
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
        let picked = restrict_list_by_selectors(&list, &selector).expect("a matchable selector");
        let picked = picked.load_control_limit_data.expect("entries");
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].limit_id, Some(LoadControlLimitId(2)));

        let all = restrict_list_by_selectors(&list, &LoadControlLimitListDataSelectors::default())
            .expect("a matchable selector");
        assert_eq!(
            all.load_control_limit_data.expect("entries").len(),
            2,
            "an empty selector selects everything"
        );
    }

    /// SPINE §5.3.4.3: a delete carries the same filters a read does.
    ///
    /// The selectors say which entries; the elements say which parts of them. LPC UC TS
    /// §3.4.1.4 uses the second to withdraw a limit's `endTime` — removing the whole
    /// entry instead lifts the curtailment rather than shortening it.
    #[test]
    fn a_delete_filter_addresses_entries_and_elements_separately() {
        let elements = LoadControlLimitDataElements {
            time_period: Some(TimePeriodElements {
                end_time: Some(crate::codec::ElementTag),
                ..Default::default()
            }),
            ..Default::default()
        };
        let only_two = LoadControlLimitListDataSelectors {
            limit_id: Some(LoadControlLimitId(2)),
        };

        let mut list = stored();
        for entry in list.load_control_limit_data.as_mut().unwrap() {
            entry.time_period = Some(TimePeriod {
                start_time: Some("2026-08-30T10:00:00Z".into()),
                end_time: Some("2026-08-30T11:00:00Z".into()),
            });
        }

        // Selectors alone: the entry goes.
        let mut entries_only = list.clone();
        delete_addressed(&mut entries_only, addresses_selected(&only_two));
        let kept = entries_only.load_control_limit_data.expect("entries");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].limit_id, Some(LoadControlLimitId(1)));

        // Selectors and elements: the entry stays and the element goes, from that entry
        // and no other.
        clear_addressed(&mut list, addresses_selected(&only_two), &elements);
        let kept = list.load_control_limit_data.expect("entries");
        assert_eq!(kept.len(), 2, "no entry was removed");
        let two = kept
            .iter()
            .find(|e| e.limit_id == Some(LoadControlLimitId(2)))
            .expect("the addressed entry");
        assert!(two.time_period.as_ref().unwrap().end_time.is_none());
        assert!(two.time_period.as_ref().unwrap().start_time.is_some());
        assert_eq!(two.limit_id, Some(LoadControlLimitId(2)), "identity kept");
        let one = kept
            .iter()
            .find(|e| e.limit_id == Some(LoadControlLimitId(1)))
            .expect("the untouched entry");
        assert!(one.time_period.as_ref().unwrap().end_time.is_some());
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
