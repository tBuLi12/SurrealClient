mod surreal_client;

use std::collections::HashMap;

use futures_util::{lock::Mutex, Stream};
use surreal_client::SurrealClient;

static client: SurrealClient = SurrealClient::new();
static d: Mutex<HashMap<String, String>> = Mutex::new(HashMap::new());

#[tokio::main]
async fn main() {
    let mut connected = client.connect().await;

    connected.query("SELECT * FROM me");

    // println!("{:#?}", value);
}
