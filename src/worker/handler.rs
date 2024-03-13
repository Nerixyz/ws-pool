use tokio::sync::mpsc;

use super::WsContext;
use crate::{handler::WorkerCommand, EventLoopAction};

/// The handler for one websocket connection.
/// `M` is the message received on the `WebSocket` connection.
/// It must implement [`FromStr`][std::str::FromStr].
/// If you're using library types, you might need to wrap the type in a newtype.
pub trait WorkerHandler<M>: Send {
    /// Worker -> Handler
    type Event: Send;
    /// Handler -> Worker
    type Command: Send;

    /// A message has been received on the `WebSocket`.
    fn on_message(
        &mut self,
        ctx: &mut WsContext<Self::Event>,
        message: M,
    ) -> impl std::future::Future<Output = EventLoopAction> + Send;

    /// A command has been received from the [Handler][crate::Handler].
    fn on_command(
        &mut self,
        ctx: &mut WsContext<Self::Event>,
        command: Self::Command,
    ) -> impl std::future::Future<Output = EventLoopAction> + Send;

    /// The handler has been created.
    /// This receives a connected `WebSocket` as well as a loopback to the command chanel,
    /// which can be used to implement timer tasks.
    /// This method won't be called when calling [`WsContext::reconnect`].
    fn on_created(
        &mut self,
        _ctx: &mut WsContext<Self::Event>,
        _loopback: Loopback<Self::Command>,
    ) -> impl std::future::Future<Output = ()> + Send {
        async {}
    }
}

/// A (weak) handle to the worker, which can be used to send commands back to the worker across tasks.
pub struct Loopback<C> {
    tx: mpsc::WeakUnboundedSender<WorkerCommand<C>>,
}

impl<C> Loopback<C> {
    pub(crate) fn new(tx: mpsc::WeakUnboundedSender<WorkerCommand<C>>) -> Self {
        Self { tx }
    }

    /// Tries to send a command to the worker.
    /// Returns true if it has been set and false if the worker is closed.
    pub fn try_send(&self, command: C) -> bool {
        let Some(locked) = self.tx.upgrade() else {
            return false;
        };

        locked.send(WorkerCommand::User(command)).is_ok()
    }
}
