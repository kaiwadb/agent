use serde_json::Value;

pub async fn execute(uri: &str, query: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let pool = sqlx::postgres::PgPool::connect(uri).await?;
    let json_query = format!("SELECT JSON_AGG(t) FROM ({}) t", query);
    let result: Option<Value> = sqlx::query_scalar(&json_query).fetch_one(&pool).await?;
    Ok(result.unwrap_or(Value::Array(vec![])))
}
