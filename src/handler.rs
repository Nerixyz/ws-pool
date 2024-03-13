use std::{collections::HashMap, fmt::Display, str::FromStr};

use crate::{
    worker::{self, WorkerHandler},
    EventLoopAction,
};
use tokio::{sync::mpsc, task::JoinHandle};
use url::Url;

/// Main trait of the libray. This manages all connections and decides what to do.
pub trait Handler {
    /// Worker -> Handler
    type Message;
    /// Handler -> Worker
    type Command;
    /// External command - from a user of a wrapping library
    type External;
    /// Events emitted by the handler - to the user of a wrapping library
    type Event;

    /// Data for a worker (in the handler)
    type WorkerData;

    /// A `message` has been received from the `worker`.
    fn on_message(
        &mut self,
        worker: &mut Worker<Self>,
        tx: &mpsc::UnboundedSender<Self::Event>,
        message: Self::Message,
    ) -> impl std::future::Future<Output = EventLoopAction> + std::marker::Send;

    /// A `worker` has stopped.
    fn on_worker_closed(
        &mut self,
        ctx: &mut HandlerContext<Self>,
        id: usize,
        data: Self::WorkerData,
    ) -> impl std::future::Future<Output = EventLoopAction> + std::marker::Send;

    /// A command to the worker was received.
    fn on_external(
        &mut self,
        ctx: &mut HandlerContext<Self>,
        external: Self::External,
    ) -> impl std::future::Future<Output = EventLoopAction> + std::marker::Send;
}

/// Full context for the handler.
/// This is the heart of a connection pool.
pub struct HandlerContext<H: Handler + ?Sized> {
    workers: HashMap<usize, Worker<H>>,
    next_id: usize,

    url: Url,

    message_tx: mpsc::Sender<WorkerEvent<H::Message>>,
    message_rx: mpsc::Receiver<WorkerEvent<H::Message>>,

    event_tx: mpsc::UnboundedSender<H::Event>,

    external_loopback_tx: mpsc::WeakUnboundedSender<H::External>,
}

pub(crate) enum WorkerEvent<T> {
    Closed(usize),
    User(usize, T),
}

pub(crate) enum WorkerCommand<T> {
    Close,
    User(T),
}

/// A worker handle in the handler.
pub struct Worker<H: Handler + ?Sized> {
    id: usize,
    tx: mpsc::UnboundedSender<WorkerCommand<H::Command>>,
    data: H::WorkerData,
}

impl<H: Handler> HandlerContext<H> {
    /// Creates a new handler context
    /// `url` is the initial URL the workers should connect to.
    /// `event_tx` a sender of events.
    /// `external_loopback_tx` is a weak sender of external events that loop back to the handler (see [`HandlerContext::loopback`]).
    #[must_use]
    pub fn new(
        url: Url,
        event_tx: mpsc::UnboundedSender<H::Event>,
        external_loopback_tx: mpsc::WeakUnboundedSender<H::External>,
    ) -> Self {
        let (message_tx, message_rx) = mpsc::channel(16);

        Self {
            workers: HashMap::new(),
            next_id: 0,
            url,
            message_tx,
            message_rx,
            event_tx,
            external_loopback_tx,
        }
    }

    /// Find a worker using the predicate `by`.
    pub fn find_mut(
        &mut self,
        by: impl Fn(usize, &H::WorkerData) -> bool,
    ) -> Option<&mut Worker<H>> {
        for (id, w) in self.workers.iter_mut() {
            if by(*id, &w.data) {
                return Some(w);
            }
        }

        None
    }

    /// Create a new worker.
    /// `data` is the data stored with the handler and `worker` is the handler for the worker.
    /// See [`WorkerHandler`].
    pub fn create<W, M>(&mut self, data: H::WorkerData, worker: W)
    where
        W: WorkerHandler<M, Event = H::Message, Command = H::Command> + 'static,
        W::Event: 'static,
        W::Command: 'static,
        M: FromStr + Send,
        M::Err: Display + Send,
    {
        self.create_with(move |_| (data, worker));
    }

    /// Create a new worker with a factory function that receives its ID and returns the worker data and handler.
    pub fn create_with<W, M>(&mut self, make: impl FnOnce(usize) -> (H::WorkerData, W))
    where
        W: WorkerHandler<M, Event = H::Message, Command = H::Command> + 'static,
        W::Event: 'static,
        W::Command: 'static,
        M: FromStr + Send,
        M::Err: Display + Send,
    {
        let id = self.next_id;
        self.next_id += 1;
        let (data, worker) = make(id);
        let tx = worker::make_connection(id, self.message_tx.clone(), self.url.clone(), worker);

        self.workers.insert(id, Worker { id, tx, data });
    }

    /// Get a loopback sender to send external messages back to the handler.
    /// This can be used when spawning tasks in `on_external` or `on_worker_closed`
    /// that need to communicate their result back to the handler.
    #[must_use]
    pub fn loopback(&self) -> mpsc::WeakUnboundedSender<H::External> {
        self.external_loopback_tx.clone()
    }

    /// Emit a handler event.
    pub fn emit(&self, event: H::Event) -> Result<(), crate::SendError> {
        self.event_tx.send(event).map_err(From::from)
    }
}

impl<H: Handler> Worker<H> {
    /// Get the ID of the worker.
    pub fn id(&self) -> usize {
        self.id
    }

    /// Get the associated data
    pub fn data(&self) -> &H::WorkerData {
        &self.data
    }

    /// Get a mutable reference to the associated data
    pub fn data_mut(&mut self) -> &mut H::WorkerData {
        &mut self.data
    }

    /// Close the worker.
    /// This will still cause the worker to show up in `on_worker_closed`.
    pub fn close(&self) -> Result<(), crate::SendError> {
        self.tx.send(WorkerCommand::Close).map_err(From::from)
    }

    /// Send a command to the worker.
    pub fn send(&self, command: H::Command) -> Result<(), crate::SendError> {
        self.tx
            .send(WorkerCommand::User(command))
            .map_err(From::from)
    }
}

/// Runs the handler task.
/// This completes when the handler is closed.
/// `ctx` should be created with [`HandlerContext::new`].
pub async fn run_handler<H>(
    mut handler: H,
    mut ctx: HandlerContext<H>,
    mut rx: mpsc::UnboundedReceiver<H::External>,
) where
    H: Handler,
{
    loop {
        let action = tokio::select! {
            Some(worker) = ctx.message_rx.recv() => {
                match worker {
                    WorkerEvent::Closed(id) => {
                        let Some(worker) = ctx.workers.remove(&id) else {
                            continue;
                        };
                        handler.on_worker_closed(&mut ctx, id, worker.data).await
                    },
                    WorkerEvent::User(id, msg) => {
                        match ctx.workers.get_mut(&id) {
                            Some(worker) => handler.on_message(worker, &ctx.event_tx, msg).await,
                            None => EventLoopAction::Continue,
                        }
                    },
                }
            },
            Some(external) = rx.recv() => {
                handler.on_external(&mut ctx, external).await
            },
            else => EventLoopAction::Stop,
        };
        match action {
            EventLoopAction::Continue => (),
            EventLoopAction::Stop => break,
        }
    }
}

/// Spawn a handler as a tokio task
pub fn spawn_handler<H>(
    handler: H,
    ctx: HandlerContext<H>,
    rx: mpsc::UnboundedReceiver<H::External>,
) -> JoinHandle<()>
where
    H: Handler + Send + 'static,
    H::External: Send,
    H::Event: Send,
    H::WorkerData: Send,
    H::Message: Send,
    H::Command: Send,
{
    tokio::spawn(async move {
        run_handler(handler, ctx, rx).await;
    })
}
