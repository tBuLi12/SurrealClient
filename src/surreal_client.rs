use futures_util::{stream::SplitSink, SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::mem;
use std::{collections::HashMap, future::Future, pin::Pin, sync::Mutex, task};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

#[derive(Deserialize)]
struct WsResponse {
    id: String,
    result: Value,
}

pub struct SurrealClient(Mutex<HashMap<String, AwaitingQuery>>);

pub struct ConnectedClient {
    awaiting: &'static Mutex<HashMap<String, AwaitingQuery>>,
    last_id: usize,
    ws: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
}

impl SurrealClient {
    pub const fn new() -> Self {
        Self(Mutex::new(HashMap::new()))
    }

    // pub fn connect<'scope, 'env>(
    //     &'env mut self,
    //     scope: &'scope Scope<'scope, 'env>,
    pub async fn connect(&'static self) -> ConnectedClient {
        let (ws_tx, mut ws_rx) = connect_async("ws://localhost:8000/rpc")
            .await
            .unwrap()
            .0
            .split();

        tokio::spawn(async move {
            while let Some(Ok(msg)) = ws_rx.next().await {
                if let Message::Text(text) = msg {
                    if let Ok(WsResponse { id, result }) = serde_json::from_str(&text) {
                        let mut awaiting = self.0.lock().unwrap();
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

        ConnectedClient {
            last_id: 0,
            ws: ws_tx,
            awaiting: &self.0,
        }
    }
}

impl ConnectedClient {
    pub fn query(&mut self, query: &str) -> FutureQueryResult {
        self.last_id += 1;

        self.ws.send(Message::text(
            json!({
                "id": self.last_id.to_string(),
                "method": "query",
                "params": [query]
            })
            .to_string(),
        ));

        FutureQueryResult {
            awaiting: self.awaiting,
            id: self.last_id,
        }
    }
}

struct AwaitingQuery {
    waker: Option<task::Waker>,
    result: Option<Value>,
}

pub struct FutureQueryResult {
    awaiting: &'static Mutex<HashMap<String, AwaitingQuery>>,
    id: usize,
}

impl Future for FutureQueryResult {
    type Output = Value;
    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> task::Poll<Self::Output> {
        {
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
        }

        task::Poll::Pending
    }
}
