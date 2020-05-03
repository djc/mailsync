use std::collections::HashMap;
use std::env;
use std::str;

use chrono::{DateTime, FixedOffset};
use email_parser::Message;
use postgres::{Client, NoTls};
use serde_derive::{Deserialize, Serialize};

use mailsync::{fuzzy_datetime_parser, Config};

fn main() {
    let args: Vec<String> = env::args().collect();
    let map = read_meta(&args[1]);
    let config = Config::from_file(&args[2]);
    println!("metadata for {} messages found", map.len());
    let conn = Client::connect(&config.store.uri, NoTls).unwrap();
    process(map, conn);
}

fn read_meta(fname: &str) -> MetaMap {
    let mut reader = csv::Reader::from_path(fname).unwrap();
    let mut map = HashMap::new();
    for result in reader.deserialize() {
        let meta: MessageMeta = result.unwrap();
        let dt = match meta.date {
            Some(ref orig) => match fuzzy_datetime_parser(orig) {
                Some(dt) => dt,
                None => {
                    continue;
                }
            },
            None => {
                continue;
                //panic!("no message-id for message with index {}", meta.seq)
            }
        };
        map.entry(dt).or_insert_with(Vec::new).push(meta);
    }
    map
}

fn process(map: MetaMap, mut conn: Client) {
    let mut matched = 0;
    let stmt = conn
        .prepare("UPDATE messages SET unid = $1, mod_seq = $2 WHERE id = $3")
        .unwrap();
    for (i, row) in conn
        .query(
            "SELECT id, raw FROM messages WHERE unid IS NULL ORDER BY id ASC",
            &[],
        )
        .unwrap()
        .iter()
        .enumerate()
    {
        if i % 10000 == 0 {
            println!("processed {} messages", i);
        }

        let id: i64 = row.get(0);
        let raw: String = row.get(1);
        let msg = Message::from_slice(raw.as_bytes());
        let headers = msg.headers();
        let snd_dt = match headers.get_first("date") {
            Some(s) => match fuzzy_datetime_parser(&s) {
                None => {
                    println!("unparsable date/time {:?} for id {}", s, id);
                    continue;
                }
                Some(dt) => dt,
            },
            None => {
                continue;
            }
        };

        let metas = match map.get(&snd_dt) {
            Some(m) => m,
            None => {
                println!(
                    "{} not found in map (id {}, mid {:?})",
                    snd_dt,
                    id,
                    headers.get_first("message-id")
                );
                continue;
            }
        };
        if metas.len() == 1 {
            let meta = metas.get(0).unwrap();
            let res = conn.execute(&stmt, &[&(meta.uid as i64), &(meta.mod_seq as i64), &id]);
            if res.is_err() {
                println!("result {:?}", res);
            } else {
                matched += 1;
            }
        } else {
            println!("unexpected value for {} = {}", snd_dt, metas.len());
        }
    }
    println!("matched {} messages based on send date", matched);
}

type MetaMap = HashMap<DateTime<FixedOffset>, Vec<MessageMeta>>;

#[derive(Deserialize, Serialize)]
struct MessageMeta {
    seq: u32,
    uid: u32,
    mod_seq: u64,
    mid: Option<String>,
    date: Option<String>,
}
