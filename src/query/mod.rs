mod mongo;
mod postgres;
use mongodb::bson::Document;
use serde::{Deserialize, Serialize};
use serde_json::{Value, from_value};
use std::time::Instant;
use syntect::{
    easy::HighlightLines, highlighting::ThemeSet, parsing::SyntaxSet, util::LinesWithEndings,
};

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

#[derive(Clone)]
pub struct QueryOptions {
    pub rich_logging: bool,
    pub max_stdout_result_length: usize,
}

impl Query {
    pub async fn execute(self, options: QueryOptions) -> Result<Value, Box<dyn std::error::Error>> {
        self.execute_with_options(options).await
    }

    pub async fn execute_with_options(
        self,
        options: QueryOptions,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        if options.rich_logging {
            self.execute_with_rich_logging(options).await
        } else {
            self.execute_with_simple_logging(options).await
        }
    }

    async fn execute_with_simple_logging(
        self,
        options: QueryOptions,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let start = Instant::now();

        match &self.engine {
            DBEngine::Mongo(_) => {
                let MongoData {
                    collection,
                    pipeline,
                } = from_value(self.data.clone())?;
                println!("Executing MongoDB query on collection: {}", collection);
                println!("Pipeline: {}", serde_json::to_string_pretty(&pipeline)?);
            }
            DBEngine::Postgres(_) => {
                let query: String = from_value(self.data.clone())?;
                println!("Executing PostgreSQL query:");
                println!("{}", query);
            }
            engine => {
                println!("Executing query on {:?}", engine);
            }
        }

        let result = match self.engine {
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
                return Err(format!("Agent cannot yet send queries to {:?}", engine).into());
            }
        };

        let duration = start.elapsed();

        match &result {
            Ok(value) => {
                let result_str = serde_json::to_string_pretty(value)?;
                let truncated = if result_str.len() > options.max_stdout_result_length {
                    format!(
                        "{}... (truncated)",
                        &result_str[..options.max_stdout_result_length]
                    )
                } else {
                    result_str
                };
                println!("Query completed in {:?}", duration);
                println!("Result: {}", truncated);
            }
            Err(e) => {
                println!("Query failed in {:?}: {}", duration, e);
            }
        }

        result
    }

    async fn execute_with_rich_logging(
        self,
        options: QueryOptions,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let start = Instant::now();

        let ps = SyntaxSet::load_defaults_newlines();
        let ts = ThemeSet::load_defaults();
        let theme = &ts.themes["base16-ocean.dark"];

        match &self.engine {
            DBEngine::Mongo(_) => {
                let MongoData {
                    collection,
                    pipeline,
                } = from_value(self.data.clone())?;

                println!("🔗 MongoDB Query");
                println!("Collection: {}", collection);
                println!("Pipeline:");
                let formatted_pipeline = serde_json::to_string_pretty(&pipeline)?;
                let highlighted = self.highlight_and_print(&formatted_pipeline, "JSON", &ps, theme);
                println!("{}", highlighted);
            }
            DBEngine::Postgres(_) => {
                let query: String = from_value(self.data.clone())?;
                println!("🐘 PostgreSQL Query:");
                let highlighted = self.highlight_and_print(&query, "SQL", &ps, theme);
                println!("{}", highlighted);
            }
            engine => {
                println!("📊 Executing query on {:?}", engine);
            }
        }

        println!("⏳ Executing query...");

        let result = match self.engine {
            DBEngine::Mongo(_) => {
                let MongoData {
                    collection,
                    pipeline,
                } = from_value(self.data.clone())?;
                mongo::execute(&self.uri, collection, pipeline).await
            }
            DBEngine::Postgres(_) => {
                let query: String = from_value(self.data.clone())?;
                postgres::execute(&self.uri, &query).await
            }
            engine => {
                return Err(format!("Agent cannot yet send queries to {:?}", engine).into());
            }
        };

        let duration = start.elapsed();

        match &result {
            Ok(value) => {
                println!("✅ Query completed in {:?}", duration);
                println!("📋 Result:");
                let result_str = serde_json::to_string_pretty(value)?;
                let truncated = if result_str.len() > options.max_stdout_result_length {
                    format!(
                        "{}... (truncated)",
                        &result_str[..options.max_stdout_result_length]
                    )
                } else {
                    result_str
                };
                let highlighted = self.highlight_and_print(&truncated, "JSON", &ps, theme);
                println!("{}", highlighted);
            }
            Err(e) => {
                println!("❌ Query failed in {:?}: {}", duration, e);
            }
        }

        result
    }

    fn highlight_and_print(
        &self,
        text: &str,
        syntax_name: &str,
        ps: &SyntaxSet,
        theme: &syntect::highlighting::Theme,
    ) -> String {
        let syntax = ps
            .find_syntax_by_name(syntax_name)
            .unwrap_or_else(|| ps.find_syntax_plain_text());

        let mut highlighter = HighlightLines::new(syntax, theme);
        let mut output = String::new();

        for line in LinesWithEndings::from(text) {
            let ranges = highlighter.highlight_line(line, ps).unwrap_or_default();
            for (style, text) in ranges {
                let color_code = format!(
                    "\x1b[38;2;{};{};{}m",
                    style.foreground.r, style.foreground.g, style.foreground.b
                );
                output.push_str(&color_code);
                output.push_str(text);
                output.push_str("\x1b[0m");
            }
        }

        output
    }
}
