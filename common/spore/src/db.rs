use std::collections::HashMap;
use std::fs;
use std::path::Path;

use rusqlite::{Connection, params};

use crate::narinfo::NarInfo;

/// src/libstore/schema.sql of Nix 2.3x, nixSchemaVersion 10.
const SCHEMA: &str = include_str!("schema.sql");

/// Opens (or creates) `<root>/nix/var/nix/db/db.sqlite` the way Nix finds it.
pub fn open(root: &Path) -> Result<Connection, String> {
    let dir = root.join("nix/var/nix/db");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let db = Connection::open(dir.join("db.sqlite")).map_err(|e| e.to_string())?;
    db.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
    // local-store.cc:getSchema parses the whole file as an int: no newline
    fs::write(dir.join("schema"), "10").map_err(|e| e.to_string())?;
    Ok(db)
}

#[cfg(test)]
fn is_valid(db: &Connection, path: &str) -> Result<bool, String> {
    db.query_row("select 1 from ValidPaths where path = ?1", [path], |_| {
        Ok(())
    })
    .map(|_| true)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(false),
        e => Err(e.to_string()),
    })
}

/// Mirrors local-store.cc:registerValidPaths: all rows first, then the references,
/// so the order of `infos` does not matter.
pub fn register(db: &mut Connection, infos: &[NarInfo]) -> Result<(), String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let tx = db.transaction().map_err(|e| e.to_string())?;
    let mut ids = HashMap::new();
    {
        let mut ins = tx
            .prepare(
                "insert into ValidPaths (path, hash, registrationTime, deriver, narSize, sigs, ca) \
                 values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .map_err(|e| e.to_string())?;
        for ni in infos {
            let id = ins
                .insert(params![
                    ni.path.to_string(),
                    format!("sha256:{}", ni.nar_hash.to_base16()),
                    now,
                    ni.deriver.as_ref().map(|d| d.to_string()),
                    ni.nar_size as i64,
                    ni.sigs.join(" "),
                    ni.ca,
                ])
                .map_err(|e| format!("{}: {e}", ni.path))?;
            ids.insert(ni.path.to_string(), id);
        }
        let mut lookup = tx
            .prepare("select id from ValidPaths where path = ?1")
            .map_err(|e| e.to_string())?;
        let mut refs = tx
            .prepare("insert or replace into Refs (referrer, reference) values (?1, ?2)")
            .map_err(|e| e.to_string())?;
        for ni in infos {
            let referrer = ids[&ni.path.to_string()];
            for r in &ni.references {
                let r = r.to_string();
                let reference = match ids.get(&r) {
                    Some(&id) => id,
                    None => lookup
                        .query_row([&r], |row| row.get(0))
                        .map_err(|_| format!("{} refers to unregistered {r}", ni.path))?,
                };
                refs.execute(params![referrer, reference])
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    tx.commit().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::hash::Sha256;
    use crate::store::path::StorePath;

    fn info(base: &str, refs: &[&str]) -> NarInfo {
        NarInfo {
            path: StorePath::from_base_name(base).unwrap(),
            url: String::new(),
            compression: "none".into(),
            nar_hash: Sha256([7; 32]),
            nar_size: 42,
            references: refs
                .iter()
                .map(|r| StorePath::from_base_name(r).unwrap())
                .collect(),
            deriver: None,
            sigs: vec!["k:sig".into()],
            ca: None,
        }
    }

    #[test]
    fn register_writes_nix_rows() {
        let root = std::env::temp_dir().join(format!("spore-db-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let a = "cp7wjv1pl4wapfk48svvizxd089v9h0a-a";
        let b = "23k009x6ahbn9whivq79llcm6207m0fb-b";
        let mut db = open(&root).unwrap();
        register(&mut db, &[info(a, &[a, b]), info(b, &[])]).unwrap();
        assert_eq!(fs::read(root.join("nix/var/nix/db/schema")).unwrap(), b"10");
        assert!(is_valid(&db, &format!("/nix/store/{b}")).unwrap());
        let hash: String = db
            .query_row(
                "select hash from ValidPaths where path = ?1",
                [format!("/nix/store/{a}")],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hash, format!("sha256:{}", "07".repeat(32)));
        let n: i64 = db
            .query_row("select count(*) from Refs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);

        let c = "g0iqacr2c3q64lhb3zq7w0rcxxyz4p8a-c";
        drop(db);
        let mut db = open(&root).unwrap();
        register(&mut db, &[info(c, &[b])]).unwrap();
        assert!(
            register(
                &mut db,
                &[info(
                    "ias8xacs1h3jy7xgwi2awvim61k2ji6c-d",
                    &["6yxih2q7hd8z4ibf2zwbgggy6hgad8gl-x"]
                )]
            )
            .is_err()
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
