//! NodeId-only navigation state.
//!
//! This is the small reusable spine from the Windows architecture: tabs track
//! where they are with opaque node ids, while path/display resolution stays in
//! `NodeStore` or platform filesystem adapters.

use crate::NodeId;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum NavigationEvent {
    NavigationCommitted(NodeId),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum NavigationIntent {
    NavigateTo(NodeId),
    NavigateUp,
    NavigateBack,
    NavigateForward,
}

#[derive(Debug, Clone)]
pub struct NavigationState {
    current: NodeId,
    back_stack: Vec<NodeId>,
    forward_stack: Vec<NodeId>,
    max_history_size: usize,
}

impl NavigationState {
    pub fn new(initial: NodeId) -> Self {
        Self {
            current: initial,
            back_stack: Vec::new(),
            forward_stack: Vec::new(),
            max_history_size: 100,
        }
    }

    pub fn current(&self) -> NodeId {
        self.current
    }

    pub fn can_go_back(&self) -> bool {
        !self.back_stack.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward_stack.is_empty()
    }

    pub fn navigate_to(&mut self, target: NodeId) -> NavigationEvent {
        if self.current != target {
            self.back_stack.push(self.current);
            if self.back_stack.len() > self.max_history_size {
                self.back_stack.remove(0);
            }
            self.forward_stack.clear();
            self.current = target;
        }
        NavigationEvent::NavigationCommitted(self.current)
    }

    pub fn replace_current(&mut self, target: NodeId) -> NavigationEvent {
        self.current = target;
        NavigationEvent::NavigationCommitted(self.current)
    }

    pub fn go_back(&mut self) -> Option<NavigationEvent> {
        let previous = self.back_stack.pop()?;
        self.forward_stack.push(self.current);
        self.current = previous;
        Some(NavigationEvent::NavigationCommitted(self.current))
    }

    pub fn go_forward(&mut self) -> Option<NavigationEvent> {
        let next = self.forward_stack.pop()?;
        self.back_stack.push(self.current);
        self.current = next;
        Some(NavigationEvent::NavigationCommitted(self.current))
    }

    pub fn emit_current(&self) -> NavigationEvent {
        NavigationEvent::NavigationCommitted(self.current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(raw: u64) -> NodeId {
        NodeId::from_raw(raw).unwrap()
    }

    #[test]
    fn navigation_is_node_id_only() {
        let mut nav = NavigationState::new(node(10));
        assert_eq!(nav.current(), node(10));

        nav.navigate_to(node(20));
        nav.navigate_to(node(30));
        assert_eq!(nav.current(), node(30));

        nav.go_back();
        assert_eq!(nav.current(), node(20));
        nav.go_forward();
        assert_eq!(nav.current(), node(30));
    }
}
