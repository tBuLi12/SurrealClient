use deadpool::{async_trait, managed};
use futures_util::stream::SplitSink;
use futures_util::Sink;
use futures_util::{SinkExt, StreamExt};
use rocket::figment::Figment;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt::Display;
use std::mem;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::{collections::HashMap, future::Future, pin::Pin, sync::Mutex, task};
use tokio::net::TcpStream;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::{self, Receiver};
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub use embedded_surreal::sql;

#[derive(Deserialize)]
struct DbResponse {
    id: String,
    result: Value,
}

type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;

pub struct SurrealClient {
    awaiting: Arc<Mutex<HashMap<String, AwaitingQuery>>>,
    last_id: AtomicUsize,
    ws: mpsc::Sender<Message>,
    sends_handle: JoinHandle<Result<(), <WsSink as Sink<Message>>::Error>>,
    recvs_handle: JoinHandle<()>,
}

impl SurrealClient {
    pub async fn connect() -> Result<Self, Error> {
        let (ws_sink, mut ws_receiver) = connect_async("ws://localhost:8000/rpc")
            .await
            .map_err(|_| Error("connection to db failed"))?
            .0
            .split();

        let (tx, rx) = mpsc::channel::<Message>(100);

        let sends_handle = tokio::spawn(Self::process_sends(rx, ws_sink));

        let awaiting = Arc::new(Mutex::new(HashMap::<String, AwaitingQuery>::new()));

        let recvs_handle = {
            let awaiting = awaiting.clone();
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
            })
        };

        let client = Self {
            awaiting,
            last_id: AtomicUsize::new(0),
            ws: tx,
            sends_handle,
            recvs_handle,
        };

        client.send("use", ["test", "test"]).await?;
        client
            .send("signin", [json!({"user": "root", "pass": "root"})])
            .await?;
        Ok(client)
    }

    pub async fn query(&self, query: &str, params: Value) -> Result<Value, Error> {
        self.send("query", json!([query, params])).await
    }

    pub async fn send<T: Serialize>(&self, method: &str, params: T) -> Result<Value, Error> {
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
            .map_err(|_| Error("connection closed"))?;

        Ok(FutureQueryResult {
            id,
            awaiting: self.awaiting.clone(),
        }
        .await)
    }

    pub async fn process_sends(
        mut rx: Receiver<Message>,
        mut ws_tx: WsSink,
    ) -> Result<(), <WsSink as Sink<Message>>::Error> {
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

#[derive(Debug)]
pub struct Error(&'static str);

impl std::error::Error for Error {}
impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "error: {}", self.0)
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

pub struct SurrealManager;

#[async_trait]
impl managed::Manager for SurrealManager {
    type Type = SurrealClient;
    type Error = Error;

    async fn create(&self) -> Result<SurrealClient, Error> {
        SurrealClient::connect().await
    }

    async fn recycle(&self, client: &mut SurrealClient) -> managed::RecycleResult<Error> {
        if client.recvs_handle.is_finished() || client.sends_handle.is_finished() {
            managed::RecycleResult::Err(managed::RecycleError::StaticMessage("connection lost"))
        } else {
            Ok(())
        }
    }
}

pub struct Pool(managed::Pool<SurrealManager>);

#[async_trait]
impl rocket_db_pools::Pool for Pool {
    type Error = Error;
    type Connection = managed::Object<SurrealManager>;

    async fn init(_: &Figment) -> Result<Self, Self::Error> {
        // let config: Config = figment.extract().map_err(|_| Error("missing config"))?;
        match managed::Pool::builder(SurrealManager {}).build() {
            Ok(pool) => Ok(Pool(pool)),
            Err(_) => Err(Error("build failed")),
        }
    }

    async fn get(&self) -> Result<Self::Connection, Self::Error> {
        self.0.get().await.map_err(|_| Error("broken connection"))
    }
}
