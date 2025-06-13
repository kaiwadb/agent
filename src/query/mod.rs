mod mongo;
mod postgres;
use mongodb::bson::Document;
use serde::{Deserialize, Serialize};
use serde_json::{Value, from_value};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Query {
    pub uri: String,
    pub engine: DBEngine,
    pub data: Value,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DBEngine {
    Mongo(MongoEngine),
    Postgres(PostgreSQLEngine),
    MySQL(MySQLEngine),
    MSSQL(MSSQLEngine),
    Oracle(OracleEngine),
    SQLite(SQLiteEngine),
    MariaDB(MariaDBEngine),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MongoEngine {
    pub version: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PostgreSQLEngine {
    pub version: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MySQLEngine {
    pub version: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MSSQLEngine {
    pub version: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct OracleEngine {
    pub version: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SQLiteEngine {
    pub version: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MariaDBEngine {
    pub version: u16,
}

#[derive(Deserialize)]
struct MongoData {
    collection: String,
    pipeline: Vec<Document>,
}

impl Query {
    pub async fn execute(self) -> Result<Value, Box<dyn std::error::Error>> {
        match self.engine {
            DBEngine::Mongo(_) => {
                let MongoData {
                    collection,
                    pipeline,
                } = from_value(self.data)?;
                mongo::execute(&self.uri, collection, pipeline).await
            }
            DBEngine::Postgres(_) => {
                let query: String = from_value(self.data)?;
                postgres::execute(&self.uri, &query).await
            }
            engine => {
                unimplemented!("Agent cannot yet send queries to {:?}", engine);
            }
        }
    }
}
