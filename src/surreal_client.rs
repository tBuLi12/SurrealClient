use futures_util::Sink;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::mem;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::{collections::HashMap, future::Future, pin::Pin, sync::Mutex, task};
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::{self, Receiver};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

#[derive(Deserialize)]
struct DbResponse {
    id: String,
    result: Value,
}

// #[derive(Serialize)]
// struct DbMessage<T: Serialize> {
//     id: usize,
//     method: &'static str,
//     params: T,
// }

pub struct SurrealClient {
    awaiting: Arc<Mutex<HashMap<String, AwaitingQuery>>>,
    last_id: AtomicUsize,
    ws: mpsc::Sender<Message>,
}

impl SurrealClient {
    pub async fn connect() -> Self {
        let (ws_sender, mut ws_receiver) = connect_async("ws://localhost:8000/rpc")
            .await
            .unwrap()
            .0
            .split();

        let (tx, rx) = mpsc::channel::<Message>(100);

        tokio::spawn(Self::process_sends(rx, ws_sender));

        let client = Self {
            awaiting: Arc::new(Mutex::new(HashMap::new())),
            last_id: AtomicUsize::new(0),
            ws: tx,
        };

        let awaiting = client.awaiting.clone();

        tokio::spawn(async move {
            while let Some(Ok(msg)) = ws_receiver.next().await {
                if let Message::Text(text) = msg {
                    if let Ok(DbResponse { id, result }) = serde_json::from_str(&text) {
                        let mut awaiting = awaiting.lock().unwrap();
                        if let Some(query) = awaiting.get_mut(&id) {
                            query.result = Some(result);
                            if let Some(waker) = mem::take(&mut query.waker) {
                                waker.wake()
                            };
                        }
                    }
                }
            }
        });

        client.send("use", ["test", "test"]).await;
        client
            .send("signin", [json!({"user": "root", "pass": "root"})])
            .await;
        client
    }

    pub async fn query(&self, query: &str) -> Value {
        self.send("query", [query]).await
    }

    pub async fn send<T: Serialize>(&self, method: &str, params: T) -> Value {
        let id = self.last_id.fetch_add(1, Ordering::AcqRel);

        self.ws
            .send(Message::text(
                json!({
                    "id": id.to_string(),
                    "method": method,
                    "params": params
                })
                .to_string(),
            ))
            .await
            .unwrap();

        FutureQueryResult {
            id,
            awaiting: self.awaiting.clone(),
        }
        .await
    }

    pub async fn process_sends<T>(mut rx: Receiver<Message>, mut ws_tx: T) -> Result<(), T::Error>
    where
        T: Sink<Message> + Unpin,
    {
        loop {
            match rx.try_recv() {
                Ok(msg) => ws_tx.feed(msg),
                Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {
                    ws_tx.flush().await?;
                    match rx.recv().await {
                        Some(msg) => ws_tx.feed(msg),
                        None => break,
                    }
                }
            }
            .await?
        }
        ws_tx.flush().await?;
        Ok(())
    }
}

struct AwaitingQuery {
    waker: Option<task::Waker>,
    result: Option<Value>,
}

pub struct FutureQueryResult {
    awaiting: Arc<Mutex<HashMap<String, AwaitingQuery>>>,
    id: usize,
}

impl Future for FutureQueryResult {
    type Output = Value;
    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> task::Poll<Self::Output> {
        let mut awaiting = self.awaiting.lock().unwrap();
        match awaiting.remove(&self.id.to_string()) {
            Some(AwaitingQuery {
                waker: _,
                result: Some(result),
            }) => return task::Poll::Ready(result),
            _ => {
                awaiting.insert(
                    self.id.to_string(),
                    AwaitingQuery {
                        waker: Some(cx.waker().clone()),
                        result: None,
                    },
                );
            }
        };

        task::Poll::Pending
    }
}
