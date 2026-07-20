use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum D2Direction {
    Up,
    #[default]
    Down,
    Left,
    Right,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Engine {
    Postgres { version: SemVer },
    Clickhouse { version: SemVer },
    BigQuery,
    MsSql { version: SemVer },
    MySQL { version: SemVer },
    /// MariaDB. Mirrors the backend `Engine::Sql(SqlEngine::MariaDB)` variant
    /// so the two crates round-trip the same JSON. Not used for connection
    /// dispatch (that goes through `ConnectionParams`); kept for metadata
    /// completeness and future version-conditional logic.
    MariaDB { version: SemVer },
    /// SQLite is compile-only for now; kept here so the tunnel can round-trip
    /// engine metadata even though it never opens a SQLite connection.
    SQLite { version: SemVer },
    Mongo { version: SemVer },
    D2 {
        #[serde(default = "default_d2_version")]
        version: SemVer,
        #[serde(default)]
        direction: D2Direction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        background_fill: Option<String>,
    },
}

fn default_d2_version() -> SemVer {
    SemVer::new(0, 7, 1)
}

#[derive(Debug, Clone)]
pub struct SemVer {
    major: u32,
    minor: u32,
    patch: u32,
}

impl SemVer {
    pub fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for SemVer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl Serialize for SemVer {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for SemVer {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(SemVerVisitor)
    }
}

struct SemVerVisitor;

impl<'de> Visitor<'de> for SemVerVisitor {
    type Value = SemVer;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a semantic version string like '1.2.3'")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        let parts: Vec<&str> = value.split('.').collect();

        if parts.len() != 3 {
            return Err(E::custom(format!(
                "expected version format 'major.minor.patch', got '{}'",
                value
            )));
        }

        let major = parts[0]
            .parse::<u32>()
            .map_err(|_| E::custom(format!("invalid major version: '{}'", parts[0])))?;

        let minor = parts[1]
            .parse::<u32>()
            .map_err(|_| E::custom(format!("invalid minor version: '{}'", parts[1])))?;

        let patch = parts[2]
            .parse::<u32>()
            .map_err(|_| E::custom(format!("invalid patch version: '{}'", parts[2])))?;

        Ok(SemVer {
            major,
            minor,
            patch,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_serialize() {
        let v = SemVer::new(1, 2, 3);
        assert_eq!(serde_json::to_string(&v).unwrap(), "\"1.2.3\"");
    }

    #[test]
    fn semver_deserialize() {
        let v: SemVer = serde_json::from_str("\"16.0.0\"").unwrap();
        assert_eq!(v.major, 16);
        assert_eq!(v.minor, 0);
        assert_eq!(v.patch, 0);
    }

    #[test]
    fn semver_deserialize_invalid() {
        assert!(serde_json::from_str::<SemVer>("\"1.2\"").is_err());
    }

    #[test]
    fn engine_postgres() {
        let json = serde_json::json!({"type": "postgres", "version": "16.0.0"});
        let e: Engine = serde_json::from_value(json).unwrap();
        assert!(matches!(e, Engine::Postgres { .. }));
    }

    #[test]
    fn engine_mongo() {
        let json = serde_json::json!({"type": "mongo", "version": "7.0.0"});
        let e: Engine = serde_json::from_value(json).unwrap();
        assert!(matches!(e, Engine::Mongo { .. }));
    }

    #[test]
    fn engine_mysql() {
        let json = serde_json::json!({"type": "mysql", "version": "8.0.0"});
        let e: Engine = serde_json::from_value(json).unwrap();
        assert!(matches!(e, Engine::MySQL { .. }));
    }

    #[test]
    fn engine_mariadb() {
        let json = serde_json::json!({"type": "mariadb", "version": "10.11.0"});
        let e: Engine = serde_json::from_value(json).unwrap();
        assert!(matches!(e, Engine::MariaDB { .. }));
    }

    #[test]
    fn engine_sqlite() {
        let json = serde_json::json!({"type": "sqlite", "version": "3.45.0"});
        let e: Engine = serde_json::from_value(json).unwrap();
        assert!(matches!(e, Engine::SQLite { .. }));
    }

    #[test]
    fn engine_clickhouse() {
        let json = serde_json::json!({"type": "clickhouse", "version": "24.0.0"});
        let e: Engine = serde_json::from_value(json).unwrap();
        assert!(matches!(e, Engine::Clickhouse { .. }));
    }

    #[test]
    fn engine_bigquery() {
        let json = serde_json::json!({"type": "bigquery"});
        let e: Engine = serde_json::from_value(json).unwrap();
        assert!(matches!(e, Engine::BigQuery));
    }

    #[test]
    fn engine_mssql() {
        let json = serde_json::json!({"type": "mssql", "version": "16.0.0"});
        let e: Engine = serde_json::from_value(json).unwrap();
        assert!(matches!(e, Engine::MsSql { .. }));
    }

    #[test]
    fn engine_d2_defaults() {
        let json = serde_json::json!({"type": "d2"});
        let e: Engine = serde_json::from_value(json).unwrap();
        match e {
            Engine::D2 {
                version,
                direction,
                background_fill,
            } => {
                assert_eq!(version.major, 0);
                assert_eq!(version.minor, 7);
                assert_eq!(version.patch, 1);
                assert_eq!(direction, D2Direction::Down);
                assert_eq!(background_fill, None);
            }
            _ => panic!("expected D2"),
        }
    }

    #[test]
    fn engine_d2_with_options() {
        let json = serde_json::json!({
            "type": "d2",
            "version": "0.7.1",
            "direction": "right",
            "background_fill": "#f5f5f5"
        });
        let e: Engine = serde_json::from_value(json).unwrap();
        match e {
            Engine::D2 {
                direction,
                background_fill,
                ..
            } => {
                assert_eq!(direction, D2Direction::Right);
                assert_eq!(background_fill, Some("#f5f5f5".to_string()));
            }
            _ => panic!("expected D2"),
        }
    }

    #[test]
    fn roundtrip_all_engines() {
        let inputs = vec![
            serde_json::json!({"type": "postgres", "version": "16.0.0"}),
            serde_json::json!({"type": "clickhouse", "version": "24.0.0"}),
            serde_json::json!({"type": "bigquery"}),
            serde_json::json!({"type": "mssql", "version": "16.0.0"}),
            serde_json::json!({"type": "mysql", "version": "8.0.0"}),
            serde_json::json!({"type": "mariadb", "version": "10.11.0"}),
            serde_json::json!({"type": "sqlite", "version": "3.45.0"}),
            serde_json::json!({"type": "mongo", "version": "7.0.0"}),
            serde_json::json!({"type": "d2", "version": "0.6.0", "direction": "down"}),
        ];

        for input in inputs {
            let engine: Engine = serde_json::from_value(input.clone()).unwrap();
            let serialized = serde_json::to_value(&engine).unwrap();
            assert_eq!(input, serialized, "roundtrip failed for {input}");
        }
    }
}
