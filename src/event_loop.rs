/// Action to perform in an event loop (break or continue)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventLoopAction {
    /// Continue with the loop
    Continue,
    /// Stop executing
    Stop,
}

impl<E> From<Result<(), E>> for EventLoopAction {
    fn from(value: Result<(), E>) -> Self {
        match value {
            Ok(()) => Self::Continue,
            Err(_) => Self::Stop,
        }
    }
}

impl EventLoopAction {
    /// Returns true if the action is [Continue][EventLoopAction::Continue].
    #[must_use]
    pub fn is_continue(self) -> bool {
        self == Self::Continue
    }

    /// Returns true if the action is [Stop][EventLoopAction::Stop].
    #[must_use]
    pub fn is_stop(self) -> bool {
        self == Self::Stop
    }
}
