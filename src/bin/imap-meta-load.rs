#[macro_use]
extern crate serde_derive;
extern crate csv;
extern crate mailsync;
extern crate postgres;
extern crate chrono;

use chrono::{DateTime, FixedOffset};

use mailsync::*;

use postgres::{Connection, TlsMode};

use std::env;
use std::str;
use std::path::PathBuf;
use std::collections::HashMap;

fn main() {
    let mut args = env::args();
    let map = read_meta(&args.nth(1).unwrap());
    println!("metadata for {} messages found", map.len());
    let conn = Connection::connect("postgres://postgres@localhost:5432/mail-djc",
                                   TlsMode::None).unwrap();
    process(map, conn);
}

fn read_meta(fname: &str) -> MetaMap {
    let mut reader = csv::Reader::from_path(fname).unwrap();
    let mut map = HashMap::new();
    for result in reader.deserialize() {
        let meta: MessageMeta = result.unwrap();
        let mid = match meta.mid {
            Some(ref mid) => mid.clone(),
            None => panic!("no message-id for message with index {}", meta.seq),
        };
        map.entry(mid).or_insert(vec![]).push(meta);
    }
    map
}


fn process(map: MetaMap, conn: Connection) {
    let mut i = 0;
    let stmt = conn.prepare("UPDATE messages SET unid = $1, mod_seq = $2 WHERE id = $3").unwrap();
    for row in &conn.query("SELECT id, dt, mid FROM messages", &[]).unwrap() {

        if i % 10000 == 0 {
            println!("processed {} messages", i);
        }

        let id: i32 = row.get(0);
        let dt: Option<DateTime<FixedOffset>> = row.get(1);
        let mid: Option<String> = row.get(2);
        if mid.is_none() {
            continue;
        }

        let mid = mid.unwrap();
        if mid.ends_with("chat@gmail.com>") {
            continue;
        }

        let metas = match map.get(&mid) {
            Some(m) => m,
            None => {
                println!("mid {} not in map", mid);
                i += 1;
                continue;
            },
        };
        if metas.len() == 1 {
            let meta = metas.get(0).unwrap();
            stmt.execute(&[&meta.uid, &(meta.mod_seq as i64), &id]);
        } else {
            println!("unexpected value for {}", mid);
        }

        i += 1;
    }
}

type MetaMap = HashMap<String, Vec<MessageMeta>>;

#[derive(Deserialize, Serialize)]
struct MessageMeta {
    seq: u32,
    uid: u32,
    mod_seq: u64,
    mid: Option<String>,
    date: Option<String>,
}
