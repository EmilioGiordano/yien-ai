//! Translations between the protocol, the layout tree, the terminal engine
//! and the agent state machine.

use yien_agent as agent;
use yien_proto as proto;

pub(crate) fn palette(p: proto::TerminalPalette) -> yien_vt::Palette {
    match p {
        proto::TerminalPalette::Sumi => yien_vt::Palette::default(),
        proto::TerminalPalette::Washi => yien_vt::Palette::washi(),
    }
}

pub(crate) fn split_zone(o: proto::Orientation, before: bool) -> yien_layout::DropZone {
    match (o, before) {
        (proto::Orientation::Horizontal, false) => yien_layout::DropZone::Right,
        (proto::Orientation::Horizontal, true) => yien_layout::DropZone::Left,
        (proto::Orientation::Vertical, false) => yien_layout::DropZone::Bottom,
        (proto::Orientation::Vertical, true) => yien_layout::DropZone::Top,
    }
}

fn orientation_back(o: yien_layout::Orientation) -> proto::Orientation {
    match o {
        yien_layout::Orientation::Horizontal => proto::Orientation::Horizontal,
        yien_layout::Orientation::Vertical => proto::Orientation::Vertical,
    }
}

pub(crate) fn direction(d: proto::Direction) -> yien_layout::Direction {
    match d {
        proto::Direction::Left => yien_layout::Direction::Left,
        proto::Direction::Right => yien_layout::Direction::Right,
        proto::Direction::Up => yien_layout::Direction::Up,
        proto::Direction::Down => yien_layout::Direction::Down,
    }
}

pub(crate) fn zone(z: proto::DropZone) -> yien_layout::DropZone {
    match z {
        proto::DropZone::Left => yien_layout::DropZone::Left,
        proto::DropZone::Right => yien_layout::DropZone::Right,
        proto::DropZone::Top => yien_layout::DropZone::Top,
        proto::DropZone::Bottom => yien_layout::DropZone::Bottom,
    }
}

/// The layout as the protocol describes it; `pane` maps each leaf to its
/// pane, and a leaf without one is left out.
pub(crate) fn layout(
    shape: &yien_layout::Shape,
    pane: &impl Fn(yien_layout::PaneId) -> Option<proto::PaneId>,
) -> Option<proto::LayoutNode> {
    match shape {
        yien_layout::Shape::Pane(leaf) => pane(*leaf).map(proto::LayoutNode::Pane),
        yien_layout::Shape::Split {
            orientation,
            children,
        } => Some(proto::LayoutNode::Split {
            orientation: orientation_back(*orientation),
            children: children
                .iter()
                .filter_map(|(flex, child)| {
                    Some(proto::LayoutChild {
                        flex: *flex,
                        node: layout(child, pane)?,
                    })
                })
                .collect(),
        }),
    }
}

/// Exit codes keep their bits: Windows codes such as `0xC000013A` do not fit
/// an `i32` as a value.
pub(crate) fn exit_code_to_agent(code: Option<u32>) -> Option<i32> {
    code.map(|c| i32::from_ne_bytes(c.to_ne_bytes()))
}

fn exit_code_from_agent(code: Option<i32>) -> Option<u32> {
    code.map(|c| u32::from_ne_bytes(c.to_ne_bytes()))
}

pub(crate) fn agent_state(state: agent::AgentState) -> proto::AgentState {
    match state {
        agent::AgentState::Idle => proto::AgentState::Idle,
        agent::AgentState::Working => proto::AgentState::Working,
        agent::AgentState::Done => proto::AgentState::Done,
        agent::AgentState::NeedsYou(reason) => proto::AgentState::NeedsYou(match reason {
            agent::NeedsReason::Permission => proto::NeedsReason::Permission,
            agent::NeedsReason::Question => proto::NeedsReason::Question,
            agent::NeedsReason::Auth => proto::NeedsReason::Auth,
            agent::NeedsReason::Blocked => proto::NeedsReason::Blocked,
            agent::NeedsReason::ScreenRule => proto::NeedsReason::ScreenRule,
        }),
        agent::AgentState::Error(reason) => proto::AgentState::Error(match reason {
            agent::ErrorReason::TurnFailed => proto::ErrorReason::TurnFailed,
            agent::ErrorReason::UnexpectedExit { code } => proto::ErrorReason::UnexpectedExit {
                code: exit_code_from_agent(code),
            },
        }),
    }
}

/// The agent signal for a sequence the terminal engine found. Titles and
/// working directories are pane details, not agent signals; OSC 133 `B` (the
/// user is typing a command) has no agent meaning either. An OSC 7501 record
/// with an `id` is about a sub-session or a background task, never the pane:
/// OpenCode ends each turn with `clear:id=…` before its own `idle`.
pub(crate) fn osc_signal(signal: &yien_vt::OscSignal) -> Option<agent::OscSignal> {
    match signal {
        yien_vt::OscSignal::Title(_) | yien_vt::OscSignal::Cwd(_) => None,
        yien_vt::OscSignal::Status(status) if status.sub_session => None,
        yien_vt::OscSignal::Status(status) => {
            Some(agent::OscSignal::Status7501(agent_status(status)))
        }
        yien_vt::OscSignal::Notify { title: Some(_), .. } => Some(agent::OscSignal::Notify777),
        yien_vt::OscSignal::Notify { title: None, .. } => Some(agent::OscSignal::Notify9),
        yien_vt::OscSignal::Prompt(mark) => match mark {
            yien_vt::PromptMark::PromptStart => {
                Some(agent::OscSignal::Prompt(agent::PromptMark::PromptStart))
            }
            yien_vt::PromptMark::CommandStart => None,
            yien_vt::PromptMark::CommandExecuted => {
                Some(agent::OscSignal::Prompt(agent::PromptMark::CommandStart))
            }
            yien_vt::PromptMark::CommandFinished { .. } => {
                Some(agent::OscSignal::Prompt(agent::PromptMark::CommandEnd))
            }
        },
    }
}

fn agent_status(status: &yien_vt::ProgramStatus) -> agent::AgentStatus {
    match status.state {
        yien_vt::ProgramState::Idle => agent::AgentStatus::Idle,
        yien_vt::ProgramState::Working => agent::AgentStatus::Working,
        yien_vt::ProgramState::Blocked => {
            agent::AgentStatus::Blocked(status.kind.map(|kind| match kind {
                yien_vt::StatusKind::Permission => agent::BlockedKind::Permission,
                yien_vt::StatusKind::Question => agent::BlockedKind::Question,
                yien_vt::StatusKind::Auth => agent::BlockedKind::Auth,
            }))
        }
        yien_vt::ProgramState::Done => agent::AgentStatus::Done,
        yien_vt::ProgramState::Error => agent::AgentStatus::Error,
        yien_vt::ProgramState::Clear => agent::AgentStatus::Clear,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc_133_b_is_not_a_running_command() {
        let typing = yien_vt::OscSignal::Prompt(yien_vt::PromptMark::CommandStart);
        assert_eq!(osc_signal(&typing), None);
        let running = yien_vt::OscSignal::Prompt(yien_vt::PromptMark::CommandExecuted);
        assert_eq!(
            osc_signal(&running),
            Some(agent::OscSignal::Prompt(agent::PromptMark::CommandStart))
        );
        let done =
            yien_vt::OscSignal::Prompt(yien_vt::PromptMark::CommandFinished { exit_code: Some(1) });
        assert_eq!(
            osc_signal(&done),
            Some(agent::OscSignal::Prompt(agent::PromptMark::CommandEnd))
        );
    }

    #[test]
    fn notifications_keep_their_source_and_titles_are_not_signals() {
        let osc9 = yien_vt::OscSignal::Notify {
            title: None,
            body: "x".into(),
        };
        let osc777 = yien_vt::OscSignal::Notify {
            title: Some("t".into()),
            body: "x".into(),
        };
        assert_eq!(osc_signal(&osc9), Some(agent::OscSignal::Notify9));
        assert_eq!(osc_signal(&osc777), Some(agent::OscSignal::Notify777));
        assert_eq!(osc_signal(&yien_vt::OscSignal::Title("t".into())), None);
        assert_eq!(osc_signal(&yien_vt::OscSignal::Cwd("/".into())), None);
    }

    fn status(
        state: yien_vt::ProgramState,
        kind: Option<yien_vt::StatusKind>,
        sub_session: bool,
    ) -> yien_vt::OscSignal {
        yien_vt::OscSignal::Status(yien_vt::ProgramStatus {
            state,
            kind,
            sub_session,
        })
    }

    #[test]
    fn rf_age_05_osc_7501_states_and_kinds_reach_the_machine() {
        use agent::{AgentStatus as A, BlockedKind as K};
        use yien_vt::{ProgramState as P, StatusKind as V};
        for (state, kind, expected) in [
            (P::Idle, None, A::Idle),
            (P::Working, None, A::Working),
            (P::Blocked, None, A::Blocked(None)),
            (
                P::Blocked,
                Some(V::Permission),
                A::Blocked(Some(K::Permission)),
            ),
            (P::Blocked, Some(V::Question), A::Blocked(Some(K::Question))),
            (P::Blocked, Some(V::Auth), A::Blocked(Some(K::Auth))),
            (P::Done, None, A::Done),
            (P::Error, None, A::Error),
            (P::Clear, None, A::Clear),
        ] {
            assert_eq!(
                osc_signal(&status(state, kind, false)),
                Some(agent::OscSignal::Status7501(expected))
            );
        }
    }

    #[test]
    fn rf_age_05_osc_7501_records_with_an_id_are_not_the_pane() {
        for state in [
            yien_vt::ProgramState::Working,
            yien_vt::ProgramState::Blocked,
            yien_vt::ProgramState::Clear,
        ] {
            assert_eq!(osc_signal(&status(state, None, true)), None);
        }
    }

    #[test]
    fn rf_atn_09_an_auth_wait_keeps_its_reason() {
        let state = agent_state(agent::AgentState::NeedsYou(agent::NeedsReason::Auth));
        assert_eq!(state, proto::AgentState::NeedsYou(proto::NeedsReason::Auth));
    }

    #[test]
    fn exit_codes_keep_their_bits() {
        let ctrl_c = Some(0xC000_013A);
        assert_eq!(exit_code_from_agent(exit_code_to_agent(ctrl_c)), ctrl_c);
        assert_eq!(exit_code_to_agent(Some(3)), Some(3));
        let state = agent_state(agent::AgentState::Error(
            agent::ErrorReason::UnexpectedExit {
                code: exit_code_to_agent(ctrl_c),
            },
        ));
        assert_eq!(
            state,
            proto::AgentState::Error(proto::ErrorReason::UnexpectedExit { code: ctrl_c })
        );
    }

    #[test]
    fn layout_maps_leaves_and_keeps_flex() {
        let mut tree = yien_layout::Tree::new();
        let right = tree.split(0, yien_layout::Orientation::Horizontal).unwrap();
        let node = layout(&tree.shape(), &|leaf| Some(proto::PaneId::new(leaf + 10))).unwrap();
        assert_eq!(
            node,
            proto::LayoutNode::Split {
                orientation: proto::Orientation::Horizontal,
                children: vec![
                    proto::LayoutChild {
                        flex: 0.5,
                        node: proto::LayoutNode::Pane(proto::PaneId::new(10)),
                    },
                    proto::LayoutChild {
                        flex: 0.5,
                        node: proto::LayoutNode::Pane(proto::PaneId::new(right + 10)),
                    },
                ],
            }
        );
    }
}
