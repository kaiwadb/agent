use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use serde_json::Value;
use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlRow};
use sqlx::{Column, Row, TypeInfo};
use tracing::info;

use crate::error::TunnelError;
use crate::params::{ConnectionParams, MysqlSslMode};

static POOLS: LazyLock<Mutex<HashMap<String, MySqlPool>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Extracted fields shared by `ConnectionParams::{Mysql, Mariadb}`. Both
/// speak the MySQL wire protocol and use the same sqlx driver, so we
/// normalise them here rather than duplicating the executor. `engine_label`
/// participates in the pool cache key so a MySQL host and a MariaDB host
/// that happen to share `host:port/database@user` still get separate pools.
struct MysqlLikeConn<'a> {
    host: &'a str,
    port: u16,
    username: &'a str,
    password: &'a str,
    database: &'a str,
    ssl_mode: MysqlSslMode,
    engine_label: &'static str,
}

fn as_mysql_like(conn: &ConnectionParams) -> Result<MysqlLikeConn<'_>, TunnelError> {
    match conn {
        ConnectionParams::Mysql {
            host,
            port,
            username,
            password,
            database,
            ssl_mode,
        } => Ok(MysqlLikeConn {
            host,
            port: *port,
            username,
            password,
            database,
            ssl_mode: *ssl_mode,
            engine_label: "mysql",
        }),
        ConnectionParams::Mariadb {
            host,
            port,
            username,
            password,
            database,
            ssl_mode,
        } => Ok(MysqlLikeConn {
            host,
            port: *port,
            username,
            password,
            database,
            ssl_mode: *ssl_mode,
            engine_label: "mariadb",
        }),
        _ => Err(TunnelError::Connection(
            "mysql executor received non-mysql/mariadb connection params".into(),
        )),
    }
}

pub async fn execute(conn: &ConnectionParams, query: &str) -> Result<Value, TunnelError> {
    let MysqlLikeConn {
        host,
        port,
        username,
        password,
        database,
        ssl_mode,
        engine_label,
    } = as_mysql_like(conn)?;

    let cache_key = format!("{engine_label}://{host}:{port}/{database}@{username}");
    let pool = {
        let pools = POOLS.lock().unwrap();
        pools.get(&cache_key).cloned()
    };

    let pool = match pool {
        Some(pool) => pool,
        None => {
            info!(engine = engine_label, host = %host, port = port, database = %database, "creating new mysql-family connection pool");
            let options = MySqlConnectOptions::new()
                .host(host)
                .port(port)
                .username(username)
                .password(password)
                .database(database)
                .ssl_mode(ssl_mode.to_sqlx());
            let pool = MySqlPoolOptions::new().connect_with(options).await?;
            POOLS.lock().unwrap().insert(cache_key, pool.clone());
            pool
        }
    };

    let rows: Vec<MySqlRow> = sqlx::query(query).fetch_all(&pool).await?;

    let result: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mut obj = serde_json::Map::new();
            for (i, col) in row.columns().iter().enumerate() {
                let val = row_value_to_json(row, i, col.type_info().name());
                obj.insert(col.name().to_string(), val);
            }
            Value::Object(obj)
        })
        .collect();

    Ok(Value::Array(result))
}

fn row_value_to_json(row: &MySqlRow, index: usize, type_name: &str) -> Value {
    if type_name == "BOOLEAN" {
        return row
            .try_get::<Option<bool>, _>(index)
            .ok()
            .flatten()
            .map_or(Value::Null, Value::from);
    }
    if type_name.contains("INT") {
        if type_name.contains("UNSIGNED") {
            return row
                .try_get::<Option<u64>, _>(index)
                .ok()
                .flatten()
                .map_or(Value::Null, Value::from);
        }
        return row
            .try_get::<Option<i64>, _>(index)
            .ok()
            .flatten()
            .map_or(Value::Null, Value::from);
    }
    if type_name == "FLOAT" || type_name == "DOUBLE" {
        return row
            .try_get::<Option<f64>, _>(index)
            .ok()
            .flatten()
            .map_or(Value::Null, Value::from);
    }
    if type_name == "JSON" {
        return row
            .try_get::<Option<String>, _>(index)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null);
    }
    // DECIMAL, VARCHAR, TEXT, DATE, DATETIME, TIMESTAMP, etc.
    row.try_get::<Option<String>, _>(index)
        .ok()
        .flatten()
        .map_or(Value::Null, Value::from)
}
