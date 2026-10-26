//! How "move tab to window" carries out a move.
//!
//! A tab from a mux server (a client domain) is grouped into windows by that
//! server. Moving it only in the GUI would make the GUI disagree with the
//! server, and the next attach or resync would put it back or split it. So
//! such a move goes to the server as one `MovePaneToNewTab`, and moves the
//! server cannot express are refused with a message instead of done locally.
//! Tabs of local domains keep the GUI-only move.
//!
//! This module holds only the decision, so it can be tested without a GUI.

use std::collections::HashSet;

/// The tab the user wants to move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabMoveSource {
    /// Every pane belongs to a local domain.
    Local,
    /// At least one pane belongs to the client domain `domain`.
    Remote { domain: usize, pane_count: usize },
}

/// The window a tab from a mux server would move into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabMoveTargetWindow {
    /// Tabs in the target whose panes are not all from the source domain.
    pub foreign_tabs: usize,
    /// Server windows of the source domain that the target shows. `None`
    /// when one of its tabs has no known server window.
    pub remote_windows: Option<HashSet<usize>>,
}

/// Where the tab goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabMoveTarget {
    NewWindow,
    Window(TabMoveTargetWindow),
}

/// How to carry out an allowed move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabMoveRoute {
    /// Move the tab between GUI windows only.
    Local,
    /// Ask the mux server to move the tab's single pane. With
    /// `remote_window: None` the server opens a new server window.
    Server { remote_window: Option<usize> },
}

pub const MULTI_PANE_REFUSAL: &str = "This tab has several panes and comes from a mux server. \
     The server can only move one pane at a time, so moving it would split the tab. \
     The tab was not moved.";

pub const FOREIGN_TARGET_REFUSAL: &str = "The target window shows tabs from another domain. \
     A mux server tab can only move into a window that shows tabs of its own server. \
     The tab was not moved.";

pub const AMBIGUOUS_TARGET_REFUSAL: &str = "The target window does not show exactly one \
     server window, so the server cannot tell where the tab belongs. The tab was not moved.";

/// Decide how to move `source` to `target`, or why the move is refused.
pub fn route_tab_move(
    source: TabMoveSource,
    target: &TabMoveTarget,
) -> Result<TabMoveRoute, &'static str> {
    let pane_count = match source {
        TabMoveSource::Local => return Ok(TabMoveRoute::Local),
        TabMoveSource::Remote { pane_count, .. } => pane_count,
    };
    if pane_count != 1 {
        return Err(MULTI_PANE_REFUSAL);
    }
    match target {
        TabMoveTarget::NewWindow => Ok(TabMoveRoute::Server {
            remote_window: None,
        }),
        TabMoveTarget::Window(window) => {
            if window.foreign_tabs > 0 {
                return Err(FOREIGN_TARGET_REFUSAL);
            }
            match window.remote_windows.as_ref() {
                Some(windows) if windows.len() == 1 => Ok(TabMoveRoute::Server {
                    remote_window: windows.iter().next().copied(),
                }),
                _ => Err(AMBIGUOUS_TARGET_REFUSAL),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(pane_count: usize) -> TabMoveSource {
        TabMoveSource::Remote {
            domain: 3,
            pane_count,
        }
    }

    fn window(foreign_tabs: usize, remote_windows: Option<&[usize]>) -> TabMoveTarget {
        TabMoveTarget::Window(TabMoveTargetWindow {
            foreign_tabs,
            remote_windows: remote_windows.map(|ids| ids.iter().copied().collect()),
        })
    }

    #[test]
    fn local_tabs_keep_the_gui_only_move() {
        assert_eq!(
            route_tab_move(TabMoveSource::Local, &TabMoveTarget::NewWindow),
            Ok(TabMoveRoute::Local)
        );
        assert_eq!(
            route_tab_move(TabMoveSource::Local, &window(2, None)),
            Ok(TabMoveRoute::Local)
        );
    }

    #[test]
    fn a_remote_tab_to_a_new_window_asks_the_server_for_a_new_server_window() {
        assert_eq!(
            route_tab_move(remote(1), &TabMoveTarget::NewWindow),
            Ok(TabMoveRoute::Server {
                remote_window: None
            })
        );
    }

    #[test]
    fn a_remote_tab_moves_into_the_one_server_window_the_target_shows() {
        assert_eq!(
            route_tab_move(remote(1), &window(0, Some(&[7]))),
            Ok(TabMoveRoute::Server {
                remote_window: Some(7)
            })
        );
    }

    #[test]
    fn a_multi_pane_remote_tab_is_refused() {
        assert_eq!(
            route_tab_move(remote(2), &TabMoveTarget::NewWindow),
            Err(MULTI_PANE_REFUSAL)
        );
        assert_eq!(
            route_tab_move(remote(3), &window(0, Some(&[7]))),
            Err(MULTI_PANE_REFUSAL)
        );
    }

    #[test]
    fn a_cross_domain_or_ambiguous_target_is_refused() {
        assert_eq!(
            route_tab_move(remote(1), &window(1, Some(&[7]))),
            Err(FOREIGN_TARGET_REFUSAL)
        );
        assert_eq!(
            route_tab_move(remote(1), &window(0, Some(&[7, 8]))),
            Err(AMBIGUOUS_TARGET_REFUSAL)
        );
        assert_eq!(
            route_tab_move(remote(1), &window(0, Some(&[]))),
            Err(AMBIGUOUS_TARGET_REFUSAL)
        );
        assert_eq!(
            route_tab_move(remote(1), &window(0, None)),
            Err(AMBIGUOUS_TARGET_REFUSAL)
        );
    }
}
