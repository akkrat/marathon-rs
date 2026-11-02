//! Port of core/src/main/kotlin/com/malinskiy/marathon/actor/StateMachine.kt
mod support;
use marathon_core::{error::OperationError, state_machine::*};
use std::{cell::RefCell, rc::Rc};
#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    Idle,
    Busy,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    Start,
    Stay,
    Unknown,
}
#[test]
fn stores_target_before_exit_enter_transition_notification() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let exit = seen.clone();
    let enter = seen.clone();
    let notify = seen.clone();
    let mut machine = StateMachine::<_, _, ()>::create(State::Idle, |graph| {
        graph.define(
            |s| matches!(s, State::Idle),
            |state| {
                state.on(|e| matches!(e, Event::Start), |_, _| transition_to(State::Busy, None));
                state.on_exit(move |_, _| exit.borrow_mut().push("exit"));
            },
        );
        graph.define(
            |s| matches!(s, State::Busy),
            |state| {
                state.on_enter(move |s, _| {
                    assert_eq!(*s, State::Busy);
                    enter.borrow_mut().push("enter");
                });
            },
        );
        graph.on_transition(move |_, state| {
            assert_eq!(*state, State::Busy);
            notify.borrow_mut().push("transition");
        });
    });
    assert!(matches!(
        machine.transition(Event::Start),
        Ok(Transition::Valid {
            to_state: State::Busy,
            ..
        })
    ));
    assert_eq!(*seen.borrow(), ["exit", "enter", "transition"]);
}
#[test]
fn first_matching_definition_and_transition_win() {
    let mut machine = StateMachine::<_, _, i32>::create(State::Idle, |graph| {
        graph.define(
            |_| true,
            |state| {
                state.on(|_| true, |s, _| dont_transition(s, Some(1)));
                state.on(|_| true, |_, _| transition_to(State::Busy, Some(2)));
            },
        );
        graph.define(
            |_| true,
            |state| state.on(|_| true, |_, _| transition_to(State::Busy, Some(3))),
        );
    });
    assert!(matches!(
        machine.transition(Event::Start),
        Ok(Transition::Valid {
            to_state: State::Idle,
            side_effect: Some(1),
            ..
        })
    ));
}
#[test]
fn invalid_transition_still_notifies_listener() {
    let count = Rc::new(RefCell::new(0));
    let observed = count.clone();
    let mut machine = StateMachine::<_, _, ()>::create(State::Idle, |graph| {
        graph.define(|_| true, |_| {});
        graph.on_transition(move |transition, state| {
            assert!(matches!(transition, Transition::Invalid { .. }));
            assert_eq!(*state, State::Idle);
            *observed.borrow_mut() += 1;
        });
    });
    assert!(matches!(
        machine.transition(Event::Unknown),
        Ok(Transition::Invalid { .. })
    ));
    assert_eq!(*count.borrow(), 1);
}
#[test]
fn missing_definition_is_kotlin_failure_and_never_panics() {
    let mut machine = StateMachine::<_, _, ()>::create(State::Idle, |_| {});
    assert!(
        matches!(machine.transition(Event::Unknown),Err(OperationError::Failed(f)) if f.class=="java.lang.IllegalStateException".into())
    );
}
#[test]
fn dont_transition_is_valid_and_keeps_state_with_side_effect() {
    let mut machine = StateMachine::create(State::Idle, |graph| {
        graph.define(
            |_| true,
            |state| state.on(|e| matches!(e, Event::Stay), |s, _| dont_transition(s, Some("action"))),
        )
    });
    assert!(matches!(
        machine.transition(Event::Stay),
        Ok(Transition::Valid {
            side_effect: Some("action"),
            ..
        })
    ));
    assert_eq!(*machine.state(), State::Idle);
}
