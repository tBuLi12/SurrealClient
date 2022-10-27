mod surreal_client;

use rocket::{get, post, routes, State};
use surreal_client::SurrealClient;

type Db = State<SurrealClient>;

#[get("/")]
async fn hello(db: &Db) -> String {
    db.query("SELECT * FROM dupa").await.to_string()
}

#[get("/add")]
async fn add(db: &Db) -> String {
    db.query("CREATE dupa:one SET name = 'sus'")
        .await
        .to_string()
}

#[rocket::main]
async fn main() -> Result<(), rocket::Error> {
    let client = SurrealClient::connect().await;

    let _ = rocket::build()
        .manage(client)
        .mount("/", routes![hello, add])
        .ignite()
        .await?
        .launch()
        .await?;

    Ok(())
}
