//! Explicit, read-only loading of user-owned LittleNavMap databases.
//! No discovery, network access, database creation, or redistribution of data.

use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

const MAX_FACILITIES: usize = 250_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Airport,
    Vor,
    Ndb,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Airport => "Airport",
            Self::Vor => "VOR / DME",
            Self::Ndb => "NDB",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Facility {
    pub kind: Kind,
    pub ident: String,
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Default)]
pub struct Snapshot {
    pub source: PathBuf,
    pub facilities: Vec<Facility>,
    pub cycle: Option<String>,
    pub airports: usize,
    pub navaids: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Could not read this LittleNavMap database: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("The navigation database exceeds {MAX_FACILITIES} facilities.")]
    TooLarge,
    #[error("The navigation database contains an invalid facility.")]
    InvalidFacility,
    #[error("The navigation database contains no facilities.")]
    Empty,
}

pub fn load(path: &Path) -> Result<Snapshot, Error> {
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // All tables come from the same SQLite read transaction. Do not use
    // immutable=1: LittleNavMap may legitimately be updating its database.
    let transaction = connection.transaction()?;
    let mut result = read(&transaction)?;
    result.source = path.to_path_buf();
    transaction.commit()?;
    Ok(result)
}

fn read(connection: &Connection) -> Result<Snapshot, Error> {
    let mut snapshot = Snapshot::default();
    for (table, kind) in [
        ("airport", Kind::Airport),
        ("vor", Kind::Vor),
        ("ndb", Kind::Ndb),
    ] {
        // Table names are constants; never interpolate a selected path or input.
        // Bound both row count and string allocation before materialising data.
        let mut statement = connection.prepare(&format!(
            "SELECT substr(ident,1,17), substr(coalesce(name,''),1,121), laty, lonx FROM {table} LIMIT {}",
            MAX_FACILITIES + 1
        ))?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            if snapshot.facilities.len() == MAX_FACILITIES {
                return Err(Error::TooLarge);
            }
            let facility = Facility {
                kind,
                ident: row.get(0)?,
                name: row.get(1)?,
                latitude: row.get(2)?,
                longitude: row.get(3)?,
            };
            if facility.ident.is_empty()
                || facility.ident.chars().count() > 16
                || facility.ident.chars().any(char::is_control)
                || facility.name.chars().count() > 120
                || facility.name.chars().any(char::is_control)
                || !facility.latitude.is_finite()
                || !facility.longitude.is_finite()
                || !(-90.0..=90.0).contains(&facility.latitude)
                || !(-180.0..=180.0).contains(&facility.longitude)
            {
                return Err(Error::InvalidFacility);
            }
            if kind == Kind::Airport {
                snapshot.airports += 1;
            } else {
                snapshot.navaids += 1;
            }
            snapshot.facilities.push(facility);
        }
    }
    if snapshot.facilities.is_empty() {
        return Err(Error::Empty);
    }
    // Cycle metadata is optional in simulator-generated databases.
    snapshot.cycle = connection
        .query_row(
            "SELECT substr(airac_cycle,1,16) FROM metadata LIMIT 1",
            [],
            |row| row.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
        .filter(|s| !s.is_empty() && !s.chars().any(char::is_control));
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE airport(ident TEXT,name TEXT,laty REAL,lonx REAL);
            CREATE TABLE vor(ident TEXT,name TEXT,laty REAL,lonx REAL);
            CREATE TABLE ndb(ident TEXT,name TEXT,laty REAL,lonx REAL);
            INSERT INTO airport VALUES('TEST','Test field',47.5,-122.3);
            INSERT INTO vor VALUES('TVR','Test VOR',47.6,-122.4);
            INSERT INTO ndb VALUES('TNB','Test NDB',47.7,-122.5);",
        )
        .unwrap();
        c
    }

    #[test]
    fn reads_all_three_kinds_without_requiring_metadata() {
        let s = read(&fixture()).unwrap();
        assert_eq!((s.airports, s.navaids), (1, 2));
        assert_eq!(s.facilities[1].kind, Kind::Vor);
        assert_eq!(s.facilities[2].kind, Kind::Ndb);
        assert!(s.cycle.is_none());
    }

    #[test]
    fn refuses_invalid_coordinates_and_overlong_identifiers() {
        for sql in [
            "UPDATE airport SET laty=91",
            "UPDATE airport SET lonx=-181",
            "UPDATE airport SET ident='abcdefghijklmnopq'",
        ] {
            let c = fixture();
            c.execute_batch(sql).unwrap();
            assert!(matches!(read(&c), Err(Error::InvalidFacility)));
        }
    }

    #[test]
    fn incompatible_and_empty_databases_are_errors() {
        assert!(matches!(
            read(&Connection::open_in_memory().unwrap()),
            Err(Error::Database(_))
        ));
        let c = fixture();
        c.execute_batch("DELETE FROM airport; DELETE FROM vor; DELETE FROM ndb;")
            .unwrap();
        assert!(matches!(read(&c), Err(Error::Empty)));
    }

    #[test]
    fn excessive_databases_are_refused_before_loading_every_row() {
        let c = fixture();
        c.execute_batch(&format!("WITH RECURSIVE numbers(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM numbers WHERE n < {MAX_FACILITIES})
            INSERT INTO airport SELECT 'TEST','Test',0,0 FROM numbers;")).unwrap();
        assert!(matches!(read(&c), Err(Error::TooLarge)));
    }

    #[test]
    fn loading_is_read_only_and_missing_files_are_not_created() {
        let path = std::env::temp_dir().join(format!("hangar-nav-{}.sqlite", std::process::id()));
        assert!(!path.exists());
        assert!(load(&path).is_err());
        assert!(!path.exists());
        let c = fixture();
        c.execute("VACUUM INTO ?", [path.to_str().unwrap()])
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        assert_eq!(load(&path).unwrap().airports, 1);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_file(path).unwrap();
    }
}
