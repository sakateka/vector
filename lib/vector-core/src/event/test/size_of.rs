use std::mem;

use lookup::{PathPrefix, path};
use quickcheck::{Arbitrary, Gen, QuickCheck, TestResult};
use vector_common::byte_size_of::ByteSizeOf;

use super::*;
use crate::event::arbitrary_impl::Name;

#[test]
fn at_least_wrapper_size() {
    // The byte size of an `Event` should always be at least as big as the
    // mem::size_of of the `Event`.
    #[allow(clippy::needless_pass_by_value)]
    fn inner(event: Event) -> TestResult {
        let baseline = mem::size_of::<Event>();
        assert!(baseline <= event.size_of());
        TestResult::passed()
    }

    QuickCheck::new()
        .tests(1_000)
        .max_tests(10_000)
        .quickcheck(inner as fn(Event) -> TestResult);
}

#[test]
fn exactly_equal_if_no_allocated_bytes() {
    // The byte size of an `Event` should always be exactly equal to its
    // `mem::size_of` if there are no reported allocated bytes.
    #[allow(clippy::needless_pass_by_value)]
    fn inner(event: Event) -> TestResult {
        let allocated_sz = event.allocated_bytes();
        if allocated_sz == 0 {
            let baseline = mem::size_of::<Event>();
            assert_eq!(baseline, event.size_of());
            return TestResult::passed();
        }
        TestResult::discard()
    }

    QuickCheck::new()
        .tests(1_000)
        .max_tests(10_000)
        .quickcheck(inner as fn(Event) -> TestResult);
}

#[test]
fn size_greater_than_allocated_size() {
    // The total byte size of an `Event` should always be strictly greater than
    // the allocated bytes of the `Event`.
    #[allow(clippy::needless_pass_by_value)]
    fn inner(event: Event) -> TestResult {
        let total_sz = event.size_of();
        let allocated_sz = event.allocated_bytes();

        assert!(total_sz > allocated_sz);
        TestResult::passed()
    }

    QuickCheck::new()
        .tests(1_000)
        .max_tests(10_000)
        .quickcheck(inner as fn(Event) -> TestResult);
}

//
// Log Events
//

/// The action that our model interpreter loop will take.
#[derive(Debug, Clone)]
pub(crate) enum Action {
    Contains {
        key: KeyString,
    },
    SizeOf,
    /// Insert a key/value pair into the [`LogEvent`]
    InsertFlat {
        key: KeyString,
        value: Value,
    },
    Remove {
        key: KeyString,
    },
}

impl Arbitrary for Action {
    fn arbitrary(g: &mut Gen) -> Self {
        match u8::arbitrary(g) % 4 {
            0 => Action::InsertFlat {
                key: String::from(Name::arbitrary(g)).into(),
                value: Value::arbitrary(g),
            },
            1 => Action::SizeOf,
            2 => Action::Contains {
                key: String::from(Name::arbitrary(g)).into(),
            },
            3 => Action::Remove {
                key: String::from(Name::arbitrary(g)).into(),
            },
            _ => unreachable!(),
        }
    }
}

#[test]
fn log_operation_maintains_size() {
    // Compare the cached LogEvent charge with a separately mutated Value. Tree
    // splits, retained roots, and spare capacity may change the charge on writes.
    fn inner(actions: Vec<Action>, initial: LogEvent) -> TestResult {
        let value = initial.value().clone();
        let mut expected = value.clone();
        let mut log_event = LogEvent::from(value.clone());
        let wrapper_bytes = log_event.size_of() - expected.allocated_bytes();

        for action in actions {
            match action {
                Action::InsertFlat { key, value } => {
                    expected.insert(path!(key.as_str()), value.clone());
                    log_event.insert((PathPrefix::Event, path!(key.as_str())), value.clone());
                }
                Action::SizeOf => {
                    assert_eq!(
                        wrapper_bytes + expected.allocated_bytes(),
                        log_event.size_of()
                    );
                }
                Action::Contains { key } => {
                    assert_eq!(
                        expected.get(path!(key.as_str())).is_some(),
                        log_event.contains((PathPrefix::Event, path!(key.as_str())))
                    );
                }
                Action::Remove { key } => {
                    expected.remove(path!(key.as_str()), false);
                    log_event.remove((PathPrefix::Event, path!(key.as_str())));
                }
            }
            assert_eq!(
                wrapper_bytes + expected.allocated_bytes(),
                log_event.size_of()
            );
        }

        TestResult::passed()
    }

    QuickCheck::new()
        .tests(1_000)
        .max_tests(10_000)
        .quickcheck(inner as fn(Vec<Action>, LogEvent) -> TestResult);
}
