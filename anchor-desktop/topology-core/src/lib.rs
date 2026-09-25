//! Platform-neutral display topology selection state.
//!
//! This crate intentionally has no Wayland or system-library dependencies so
//! hotplug lifecycle tests can run on every development host.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSelection {
    pub index: usize,
    pub name: String,
}

impl OutputSelection {
    pub fn new(index: usize, name: impl Into<String>) -> Self {
        Self { index, name: name.into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOutcome {
    Preserved,
    FellBack,
    NoOutputs,
}

/// Reconcile a selection against a complete, ordered topology snapshot.
///
/// Stable output names own identity. Indices are only cursors into the latest
/// snapshot and are recomputed after every disconnect, reconnect, or reorder.
pub fn reconcile_selection<'a>(
    output_names: impl IntoIterator<Item = &'a str>,
    selection: &mut OutputSelection,
) -> ReconcileOutcome {
    let names: Vec<&str> = output_names.into_iter().collect();
    if let Some(index) = names.iter().position(|name| *name == selection.name) {
        selection.index = index;
        return ReconcileOutcome::Preserved;
    }

    selection.index = 0;
    if let Some(first) = names.first() {
        selection.name = (*first).to_string();
        ReconcileOutcome::FellBack
    } else {
        selection.name.clear();
        ReconcileOutcome::NoOutputs
    }
}

/// Choose a fallback without ever returning the output being removed.
pub fn fallback_index<'a>(
    output_names: impl IntoIterator<Item = &'a str>,
    removed_name: &str,
) -> Option<usize> {
    output_names.into_iter().position(|name| name != removed_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reconcile(outputs: &[&str], selection: &mut OutputSelection) -> ReconcileOutcome {
        reconcile_selection(outputs.iter().copied(), selection)
    }

    fn ordered_topologies<'a>(outputs: &'a [&'a str]) -> Vec<Vec<&'a str>> {
        fn append<'a>(
            prefix: &mut Vec<&'a str>,
            remaining: &mut Vec<&'a str>,
            result: &mut Vec<Vec<&'a str>>,
        ) {
            result.push(prefix.clone());
            for index in 0..remaining.len() {
                let output = remaining.remove(index);
                prefix.push(output);
                append(prefix, remaining, result);
                prefix.pop();
                remaining.insert(index, output);
            }
        }

        let mut result = Vec::new();
        append(&mut Vec::new(), &mut outputs.to_vec(), &mut result);
        result
    }

    #[test]
    fn selected_screen_disconnects_then_reconnects_without_restoring_a_stale_index() {
        let mut selection = OutputSelection::new(1, "HEADLESS-1");

        assert_eq!(
            reconcile(&["eDP-1", "HEADLESS-1"], &mut selection),
            ReconcileOutcome::Preserved
        );
        assert_eq!(selection, OutputSelection::new(1, "HEADLESS-1"));

        assert_eq!(reconcile(&["eDP-1"], &mut selection), ReconcileOutcome::FellBack);
        assert_eq!(selection, OutputSelection::new(0, "eDP-1"));

        // Reconnecting the removed display must not silently steal selection
        // back from the fallback chosen while it was absent.
        assert_eq!(
            reconcile(&["HEADLESS-1", "eDP-1"], &mut selection),
            ReconcileOutcome::Preserved
        );
        assert_eq!(selection, OutputSelection::new(1, "eDP-1"));
    }

    #[test]
    fn all_screens_disconnect_then_reconnect_uses_a_valid_fallback() {
        let mut selection = OutputSelection::new(0, "HEADLESS-1");

        assert_eq!(reconcile(&[], &mut selection), ReconcileOutcome::NoOutputs);
        assert_eq!(selection, OutputSelection::new(0, ""));

        assert_eq!(reconcile(&["HDMI-A-1", "eDP-1"], &mut selection), ReconcileOutcome::FellBack);
        assert_eq!(selection, OutputSelection::new(0, "HDMI-A-1"));
    }

    #[test]
    fn reconnect_and_reorder_recompute_index_from_stable_name() {
        let mut selection = OutputSelection::new(2, "DP-2");
        let snapshots = [
            vec!["eDP-1", "HEADLESS-1", "DP-2"],
            vec!["DP-2", "eDP-1"],
            vec!["HEADLESS-1", "eDP-1", "DP-2"],
            vec!["eDP-1", "DP-2", "HEADLESS-1"],
        ];

        for outputs in snapshots {
            assert_eq!(reconcile(&outputs, &mut selection), ReconcileOutcome::Preserved);
            assert_eq!(outputs[selection.index], selection.name);
            assert_eq!(selection.name, "DP-2");
        }
    }

    #[test]
    fn repeated_disconnect_reconnect_sequences_always_leave_a_valid_selection() {
        let snapshots = [
            vec!["eDP-1", "HEADLESS-1"],
            vec!["eDP-1"],
            vec![],
            vec!["HEADLESS-2"],
            vec!["eDP-1", "HEADLESS-2"],
            vec!["HEADLESS-2", "eDP-1"],
        ];
        let mut selection = OutputSelection::new(1, "HEADLESS-1");

        for outputs in snapshots {
            let outcome = reconcile(&outputs, &mut selection);
            if outputs.is_empty() {
                assert_eq!(outcome, ReconcileOutcome::NoOutputs);
                assert_eq!(selection, OutputSelection::new(0, ""));
            } else {
                assert!(selection.index < outputs.len());
                assert_eq!(outputs[selection.index], selection.name);
            }
        }
    }

    #[test]
    fn every_ordered_topology_preserves_identity_or_uses_the_first_fallback() {
        let universe = ["eDP-1", "HEADLESS-1", "HDMI-A-1", "DP-2"];
        let topologies = ordered_topologies(&universe);

        for selected in universe.into_iter().chain(["REMOVED-1"]) {
            for outputs in &topologies {
                let mut selection = OutputSelection::new(usize::MAX, selected);
                let outcome = reconcile(outputs, &mut selection);

                if let Some(expected_index) = outputs.iter().position(|name| *name == selected) {
                    assert_eq!(outcome, ReconcileOutcome::Preserved);
                    assert_eq!(selection, OutputSelection::new(expected_index, selected));
                } else if let Some(first) = outputs.first() {
                    assert_eq!(outcome, ReconcileOutcome::FellBack);
                    assert_eq!(selection, OutputSelection::new(0, *first));
                } else {
                    assert_eq!(outcome, ReconcileOutcome::NoOutputs);
                    assert_eq!(selection, OutputSelection::new(0, ""));
                }

                let stable = selection.clone();
                let second_outcome = reconcile(outputs, &mut selection);
                assert_eq!(selection, stable);
                assert_eq!(
                    second_outcome,
                    if outputs.is_empty() {
                        ReconcileOutcome::NoOutputs
                    } else {
                        ReconcileOutcome::Preserved
                    }
                );
            }
        }
    }

    #[test]
    fn removal_fallback_never_returns_the_disconnecting_screen() {
        let outputs = ["HEADLESS-1", "eDP-1", "HDMI-A-1"];
        for removed in outputs {
            let index = fallback_index(outputs, removed).expect("another output remains");
            assert_ne!(outputs[index], removed);
        }
        assert_eq!(fallback_index(["HEADLESS-1"], "HEADLESS-1"), None);
    }
}
