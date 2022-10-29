mod surreal_client;

use rocket::{get, routes};
use rocket_db_pools::{Connection, Database};
use surreal_client::sql;

#[derive(Database)]
#[database("main")]
struct SurrealDb(surreal_client::Pool);
type Db = Connection<SurrealDb>;

#[get("/")]
async fn hello(db: Db) -> String {
    let id = "eeeeeee";
    let name = "meee";
    sql!(SELECT ..User FROM dupa:{id} WHERE name = {name})
        .await
        .unwrap()
        .to_string()
}

#[rocket::launch]
fn rocket() -> _ {
    rocket::build()
        .attach(SurrealDb::init())
        .mount("/", routes![hello])
}
