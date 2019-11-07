use std::collections::HashMap;
use std::env;
use std::str;

use chrono::{DateTime, FixedOffset};
use csv;
use email_parser::Message;
use postgres::{Client, NoTls};
use serde_derive::{Deserialize, Serialize};

use mailsync::Config;

fn main() {
    let args: Vec<String> = env::args().collect();
    let map = read_meta(&args[1]);
    let config = Config::from_file(&args[2]);
    println!("metadata for {} messages found", map.len());
    let conn = Client::connect(&config.store.uri, NoTls).unwrap();
    process(map, conn);
}

const FORMATS: [&str; 5] = [
    "%-e %b %Y %H:%M:%S %z",
    "%-e %b %Y %H:%M %z",
    "%e %b %Y %H:%M:%S %z",
    "%e %b %Y %H:%M",
    "%b %d %H:%M:%S %z %Y",
];

fn fuzzy_datetime_parser(orig: &str) -> Option<DateTime<FixedOffset>> {
    let mut s = orig.to_string();
    if &s[3..5] == ", " {
        s = s[5..].trim().to_string();
    } else if s.starts_with("Wed ") || s.starts_with("Sun ") || s.starts_with("Sat ") {
        s = s[4..].trim().to_string();
    } else if s.starts_with(", ") {
        s = s[2..].trim().to_string();
    }
    s = s.replace("GMT+00:00", "+0000");
    s = s.replace("-0060", "-0100");
    s = s.replace("-05-30", "-0530");
    s = s.replace(" t0100", " +0100");
    let mut parts = s
        .replace("  ", " ")
        .split(" ")
        .take(5)
        .map(|s| s.to_string())
        .collect::<Vec<String>>();
    if parts.len() > 3 && parts[3].contains(".") {
        let mut t = parts.remove(3);
        t = t.replace(".", ":");
        parts.insert(3, t);
    }
    if parts.len() > 4 {
        if parts[4] == "CET" {
            let _ = parts.remove(4);
            parts.insert(4, "+0100".to_string());
        } else if parts[4] == "UT" || parts[4] == "GMT" || parts[4] == "UTC" {
            let _ = parts.remove(4);
            parts.insert(4, "+0000".to_string());
        } else if parts[4] == "CEST" {
            let _ = parts.remove(4);
            parts.insert(4, "+0200".to_string());
        } else if parts[4] == "CST" {
            let _ = parts.remove(4);
            parts.insert(4, "-0600".to_string());
        } else if parts[4] == "PDT" {
            let _ = parts.remove(4);
            parts.insert(4, "-0700".to_string());
        } else if parts[4] == "PST" {
            let _ = parts.remove(4);
            parts.insert(4, "-0800".to_string());
        } else if parts[4] == "EST" {
            let _ = parts.remove(4);
            parts.insert(4, "-0500".to_string());
        } else if parts[4] == "Pacific" && orig.contains("Pacific Standard Time") {
            let _ = parts.remove(4);
            parts.insert(4, "-0800".to_string());
        } else if parts[4] == "%z" && s.contains("(PDT)") {
            let _ = parts.remove(4);
            parts.insert(4, "-0700".to_string());
        } else if parts[3] == "UTC" {
            let _ = parts.remove(3);
            parts.insert(3, "+0000".to_string());
        }
    }
    if parts.len() < 5 {
        parts.push("+0000".to_string());
    }
    let new = parts.join(" ");
    for (i, fmt) in FORMATS.iter().enumerate() {
        let parsed = DateTime::parse_from_str(&new, fmt);
        match parsed {
            Ok(dt) => return Some(dt),
            Err(_) => {
                if i == FORMATS.len() - 1 {
                    //panic!("failed to parse {}, {:?} -> {:?}", e, orig, new);
                    return None;
                }
            }
        }
    }
    unreachable!();
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
        map.entry(dt).or_insert(vec![]).push(meta);
    }
    map
}

fn process(map: MetaMap, mut conn: Client) {
    let mut i = 0;
    let mut matched = 0;
    let stmt = conn
        .prepare("UPDATE messages SET unid = $1, mod_seq = $2 WHERE id = $3")
        .unwrap();
    for row in &conn
        .query(
            "SELECT id, raw FROM messages WHERE unid IS NULL ORDER BY id ASC",
            &[],
        )
        .unwrap()
    {
        if i % 10000 == 0 {
            println!("processed {} messages", i);
        }

        i += 1;
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
