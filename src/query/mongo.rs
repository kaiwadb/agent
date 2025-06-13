use mongodb::{Client, Collection, bson::Document};
use serde_json::{Value, to_value};

pub async fn execute(
    uri: &str,
    collection: String,
    pipeline: Vec<Document>,
) -> Result<Value, Box<dyn std::error::Error>> {
    println!(
        "Connecting to MongoDB: {}",
        uri.split('@').last().unwrap_or("unknown")
    );
    let client = Client::with_uri_str(&uri).await?;
    let db = client
        .default_database()
        .ok_or("No default database specified in URI")?;
    let collection: Collection<Document> = db.collection(&collection);

    println!("Executing query...");
    let mut cursor = collection.aggregate(pipeline).await?;
    let mut batch = Vec::new();

    while cursor.advance().await? {
        let doc = cursor.deserialize_current()?;
        batch.push(doc);
    }

    Ok(to_value(&batch)?)
}
