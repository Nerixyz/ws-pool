use std::{str::FromStr, time::Duration};

use tokio::sync::mpsc;
use tracing::debug;
use url::Url;
use ws_pool::{
    spawn_handler, EventLoopAction, Handler, HandlerContext, Worker, WorkerHandler, WsContext,
};

const TOPICS_PER_CONN: usize = 5;

/// This would be the "public-facing" message type to interact with the pool
///
/// If you're developing a library, it's better to wrap the sender in a custom struct
/// and explicitly provide methods instead of providing the "raw" sender.
enum HandlerCommand {
    Subscribe(String),
}

/// This would be the "public-facing" event emitted from the pool.
enum HandlerEvent {
    Message(usize, String),
}

/// The parsed websocket message in this protocol
enum SimpleMessage {
    Open,
    Event(String),
    Reconnect(String),
}

/// Data associated with a worker in the handler.
/// We store if the connection is open and the pending topics (if it's not yet open).
struct SimpleWorkerData {
    is_open: bool,
    pending_topics: Vec<String>,
    topics: Vec<String>,
}

/// We don't have handler state other than for each connection, so this struct is empty.
struct SimpleHandler;

/// The worker doesn't have any state.
struct SimpleWorker;

/// The event sent from workers to the handler.
/// The worker-id is implicitly sent along.
enum WorkerEvent {
    Open,
    Event(String),
}
/// The command sent from the handler to workers.
enum WorkerCommand {
    Subscribe(String),
}

impl Handler for SimpleHandler {
    /// Worker -> Handler
    type Message = WorkerEvent;
    /// Handler -> Worker
    type Command = WorkerCommand;
    /// [User] -> Handler
    type External = HandlerCommand;
    /// Handler -> [User]
    type Event = HandlerEvent;

    /// Data associated with a worker
    type WorkerData = SimpleWorkerData;

    async fn on_message(
        &mut self,
        worker: &mut ws_pool::Worker<Self>,
        tx: &tokio::sync::mpsc::UnboundedSender<Self::Event>,
        message: Self::Message,
    ) -> EventLoopAction {
        match message {
            WorkerEvent::Open => {
                // Flush pending topics
                worker.data_mut().is_open = true;
                let topics = std::mem::take(&mut worker.data_mut().pending_topics);
                let action = send_all(worker, topics.iter().cloned());
                worker.data_mut().topics = topics;
                action
            }
            // Re-broadcast event to the outside
            WorkerEvent::Event(e) => tx.send(HandlerEvent::Message(worker.id(), e)).into(),
        }
    }

    async fn on_worker_closed(
        &mut self,
        ctx: &mut ws_pool::HandlerContext<Self>,
        _id: usize,
        data: Self::WorkerData,
    ) -> EventLoopAction {
        let Some(tx) = ctx.loopback().upgrade() else {
            return EventLoopAction::Stop;
        };
        // Re-subscribe to the topics.
        // We don't see this in action here.
        // If you remove the line to send "reconnect=..." in server.ts, this will run!
        for topic in data
            .topics
            .into_iter()
            .chain(data.pending_topics.into_iter())
        {
            if tx.send(HandlerCommand::Subscribe(topic)).is_err() {
                return EventLoopAction::Stop;
            }
        }
        EventLoopAction::Continue
    }

    async fn on_external(
        &mut self,
        ctx: &mut ws_pool::HandlerContext<Self>,
        external: Self::External,
    ) -> EventLoopAction {
        match external {
            HandlerCommand::Subscribe(topic) => {
                debug!(%topic, "subscribe");
                if let Some(conn) = ctx.find_mut(|_, data| data.topic_count() < TOPICS_PER_CONN) {
                    debug!(%topic, id=%conn.id(), "using existing connection");
                    if conn.data_mut().is_open {
                        conn.send(WorkerCommand::Subscribe(topic)).into()
                    } else {
                        conn.data_mut().pending_topics.push(topic);
                        EventLoopAction::Continue
                    }
                } else {
                    debug!(%topic,"creating new connection");
                    ctx.create(
                        SimpleWorkerData {
                            is_open: false,
                            pending_topics: vec![topic],
                            topics: vec![],
                        },
                        SimpleWorker,
                    );
                    EventLoopAction::Continue
                }
            }
        }
    }
}

impl SimpleWorkerData {
    /// Returns the total count of all topics in this connection
    fn topic_count(&self) -> usize {
        self.pending_topics.len() + self.topics.len()
    }
}

impl WorkerHandler<SimpleMessage> for SimpleWorker {
    /// Worker -> Handler
    type Event = WorkerEvent;
    /// Handler -> Worker
    type Command = WorkerCommand;

    async fn on_message(
        &mut self,
        ctx: &mut WsContext<Self::Event>,
        message: SimpleMessage,
    ) -> EventLoopAction {
        match message {
            SimpleMessage::Open => ctx.emit(WorkerEvent::Open).await.into(),
            SimpleMessage::Event(e) => ctx.emit(WorkerEvent::Event(e)).await.into(),
            // We're just reconnecting here, without resubscribing to the topics.
            // If your service has support for reconnects where topics are retained,
            // use this method, otherwise it's better to return Stop here and let the handler
            // redistribute the topics, as you might need to re-auth.
            SimpleMessage::Reconnect(url) => {
                let Ok(url) = Url::parse(&url) else {
                    return EventLoopAction::Stop;
                };
                ctx.reconnect(&url).await.into()
            }
        }
    }

    async fn on_command(
        &mut self,
        ctx: &mut WsContext<Self::Event>,
        command: Self::Command,
    ) -> EventLoopAction {
        match command {
            WorkerCommand::Subscribe(topic) => ctx.send(&format!("sub={topic}")).await.into(),
        }
    }
}

/// Creates a new pool.
/// As explained above, in a real library, you should return a wrapped sender to help with API compatibility.
fn create_it(
    url: Url,
) -> (
    mpsc::UnboundedSender<HandlerCommand>,
    mpsc::UnboundedReceiver<HandlerEvent>,
) {
    let (tx, external_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let ctx = HandlerContext::new(url, event_tx, tx.downgrade());

    spawn_handler(SimpleHandler, ctx, external_rx);

    (tx, event_rx)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // You can set RUST_LOG=debug to get debug logs
    tracing_subscriber::fmt::init();

    let url = std::env::args().nth(1).expect("missing url");
    let url = Url::parse(&url)?;

    let (tx, mut rx) = create_it(url);
    // keep tx alive to receive events
    let _tx = tx.clone();

    tokio::spawn(async move {
        for i in 0..(TOPICS_PER_CONN * 4) {
            println!("Subscribing to topic-{i}");
            let _ = tx.send(HandlerCommand::Subscribe(format!("topic-{i}")));
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    while let Some(msg) = rx.recv().await {
        match msg {
            HandlerEvent::Message(worker, msg) => {
                println!("[{worker}]: {msg}");
            }
        }
    }

    Ok(())
}

// Helper to parse messages from a string
#[derive(Debug, thiserror::Error)]
#[error("Failed to parse message")]
struct ParseError;

impl FromStr for SimpleMessage {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (k, v) = s.split_once('=').ok_or(ParseError)?;
        match k {
            "open" => Ok(Self::Open),
            "event" => Ok(Self::Event(v.to_owned())),
            "reconnect" => Ok(Self::Reconnect(v.to_owned())),
            _ => Err(ParseError),
        }
    }
}

// Subscribes to all topics on a worker
fn send_all(
    worker: &mut Worker<SimpleHandler>,
    topics: impl Iterator<Item = String>,
) -> EventLoopAction {
    for topic in topics {
        if worker.send(WorkerCommand::Subscribe(topic)).is_err() {
            return EventLoopAction::Stop;
        }
    }
    EventLoopAction::Continue
}
