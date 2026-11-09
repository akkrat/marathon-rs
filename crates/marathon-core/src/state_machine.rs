//! Port of core/src/main/kotlin/com/malinskiy/marathon/actor/StateMachine.kt
use marathon_error::FailureKind;
/*
 * Copyright (c) 2018, Match Group, LLC
 * All rights reserved.
 *
 * Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:
 * Redistributions of source code must retain the above copyright
notice, this list of conditions and the following disclaimer.
 * Redistributions in binary form must reproduce the above copyright
notice, this list of conditions and the following disclaimer in the
documentation and/or other materials provided with the distribution.
 * Neither the name of Match Group, LLC nor the names of its contributors
may be used to endorse or promote products derived from this software
without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL MATCH GROUP, LLC BE LIABLE FOR ANY
DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
(INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND
ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */
use crate::{error::OperationResult, executor::shared::failure};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transition<S, E, A> {
    Valid {
        from_state: S,
        event: E,
        to_state: S,
        side_effect: Option<A>,
    },
    Invalid {
        from_state: S,
        event: E,
    },
}
type Matcher<T> = Box<dyn Fn(&T) -> bool>;
type Rule<S, E, A> = (Matcher<E>, Box<dyn Fn(&S, &E) -> (S, Option<A>)>);
type Listener<S, E> = Box<dyn FnMut(&S, &E)>;
type TransitionListener<S, E, A> = Box<dyn FnMut(&Transition<S, E, A>, &S)>;
pub struct StateDefinition<S, E, A> {
    matcher: Matcher<S>,
    rules: Vec<Rule<S, E, A>>,
    enter: Vec<Listener<S, E>>,
    exit: Vec<Listener<S, E>>,
}
impl<S: Clone, E, A> StateDefinition<S, E, A> {
    pub fn on(
        &mut self,
        matcher: impl Fn(&E) -> bool + 'static,
        transition: impl Fn(&S, &E) -> (S, Option<A>) + 'static,
    ) {
        self.rules.push((Box::new(matcher), Box::new(transition)));
    }
    pub fn on_enter(&mut self, listener: impl FnMut(&S, &E) + 'static) {
        self.enter.push(Box::new(listener));
    }
    pub fn on_exit(&mut self, listener: impl FnMut(&S, &E) + 'static) {
        self.exit.push(Box::new(listener));
    }
}
pub fn transition_to<S, A>(state: S, action: Option<A>) -> (S, Option<A>) {
    (state, action)
}
pub fn dont_transition<S: Clone, A>(state: &S, action: Option<A>) -> (S, Option<A>) {
    (state.clone(), action)
}
pub struct StateMachine<S, E, A> {
    state: S,
    definitions: Vec<StateDefinition<S, E, A>>,
    listeners: Vec<TransitionListener<S, E, A>>,
}
impl<S: Clone, E: Clone, A> StateMachine<S, E, A> {
    pub fn create(initial_state: S, build: impl FnOnce(&mut Self)) -> Self {
        let mut machine = Self {
            state: initial_state,
            definitions: Vec::new(),
            listeners: Vec::new(),
        };
        build(&mut machine);
        machine
    }
    pub fn state(&self) -> &S {
        &self.state
    }
    pub fn define(
        &mut self,
        matcher: impl Fn(&S) -> bool + 'static,
        build: impl FnOnce(&mut StateDefinition<S, E, A>),
    ) {
        let mut definition = StateDefinition {
            matcher: Box::new(matcher),
            rules: Vec::new(),
            enter: Vec::new(),
            exit: Vec::new(),
        };
        build(&mut definition);
        self.definitions.push(definition);
    }
    pub fn on_transition(&mut self, listener: impl FnMut(&Transition<S, E, A>, &S) + 'static) {
        self.listeners.push(Box::new(listener));
    }
    fn definition(&self, state: &S) -> OperationResult<usize> {
        self.definitions
            .iter()
            .position(|d| (d.matcher)(state))
            .ok_or_else(|| failure(FailureKind::IllegalState, "Required value was null.").into())
    }
    /// Enum matchers replace reified Kotlin subtype dispatch (approved P2).
    /// No await, and &mut self makes listener reentrancy explicitly unavailable.
    pub fn transition(&mut self, event: E) -> OperationResult<Transition<S, E, A>> {
        let from = self.definition(&self.state)?;
        let transition = match self.definitions[from].rules.iter().find(|(matches, _)| matches(&event)) {
            Some((_, rule)) => {
                let (to_state, side_effect) = rule(&self.state, &event);
                Transition::Valid {
                    from_state: self.state.clone(),
                    event: event.clone(),
                    to_state,
                    side_effect,
                }
            }
            None => Transition::Invalid {
                from_state: self.state.clone(),
                event: event.clone(),
            },
        };
        if let Transition::Valid {
            from_state, to_state, ..
        } = &transition
        {
            self.state = to_state.clone();
            for exit in &mut self.definitions[from].exit {
                exit(from_state, &event);
            }
            let to = self.definition(&self.state)?;
            for enter in &mut self.definitions[to].enter {
                enter(&self.state, &event);
            }
        }
        for listener in &mut self.listeners {
            listener(&transition, &self.state);
        }
        Ok(transition)
    }
}
