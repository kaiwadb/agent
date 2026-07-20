use serde::{Deserialize, Serialize};

/// Wire-compatible mirror of `kaiwadb_common::database::connection::ConnectionParams`.
/// Tunnel doesn't depend on the common crate, so the type is duped here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "engine", rename_all = "lowercase")]
pub enum ConnectionParams {
    Postgres {
        host: String,
        #[serde(default = "default_pg_port")]
        port: u16,
        username: String,
        password: String,
        database: String,
        #[serde(default)]
        sslmode: PgSslMode,
    },
    Mysql {
        host: String,
        #[serde(default = "default_mysql_port")]
        port: u16,
        username: String,
        password: String,
        database: String,
        #[serde(default)]
        ssl_mode: MysqlSslMode,
    },
    /// MariaDB speaks the MySQL wire protocol and is served by the same sqlx
    /// `mysql` driver — the query executor dispatches this variant to the
    /// mysql handler. Modelled as a separate variant so the wire tag
    /// (`"engine": "mariadb"`) and the backend's `engine_matches_connection`
    /// check both remain unambiguous.
    Mariadb {
        host: String,
        #[serde(default = "default_mariadb_port")]
        port: u16,
        username: String,
        password: String,
        database: String,
        #[serde(default)]
        ssl_mode: MysqlSslMode,
    },
    Clickhouse {
        host: String,
        #[serde(default = "default_clickhouse_port")]
        port: u16,
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        password: Option<String>,
        #[serde(default)]
        database: Option<String>,
        #[serde(default)]
        secure: bool,
    },
    Mssql {
        host: String,
        #[serde(default = "default_mssql_port")]
        port: u16,
        #[serde(default)]
        instance: Option<String>,
        username: String,
        password: String,
        #[serde(default)]
        database: Option<String>,
        #[serde(default = "default_true")]
        trust_cert: bool,
    },
    Mongo {
        #[serde(default)]
        srv: bool,
        hosts: Vec<MongoHost>,
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        password: Option<String>,
        database: String,
        #[serde(default)]
        auth_source: Option<String>,
        #[serde(default)]
        replica_set: Option<String>,
        #[serde(default)]
        tls: Option<bool>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MongoHost {
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PgSslMode {
    Disable,
    Allow,
    #[default]
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MysqlSslMode {
    Disabled,
    #[default]
    Preferred,
    Required,
    VerifyCa,
    VerifyIdentity,
}

fn default_pg_port() -> u16 { 5432 }
fn default_mysql_port() -> u16 { 3306 }
fn default_mariadb_port() -> u16 { 3306 }
fn default_clickhouse_port() -> u16 { 8123 }
fn default_mssql_port() -> u16 { 1433 }
fn default_true() -> bool { true }

impl PgSslMode {
    pub fn to_sqlx(self) -> sqlx::postgres::PgSslMode {
        use sqlx::postgres::PgSslMode as S;
        match self {
            PgSslMode::Disable => S::Disable,
            PgSslMode::Allow => S::Allow,
            PgSslMode::Prefer => S::Prefer,
            PgSslMode::Require => S::Require,
            PgSslMode::VerifyCa => S::VerifyCa,
            PgSslMode::VerifyFull => S::VerifyFull,
        }
    }
}

impl MysqlSslMode {
    pub fn to_sqlx(self) -> sqlx::mysql::MySqlSslMode {
        use sqlx::mysql::MySqlSslMode as S;
        match self {
            MysqlSslMode::Disabled => S::Disabled,
            MysqlSslMode::Preferred => S::Preferred,
            MysqlSslMode::Required => S::Required,
            MysqlSslMode::VerifyCa => S::VerifyCa,
            MysqlSslMode::VerifyIdentity => S::VerifyIdentity,
        }
    }
}
