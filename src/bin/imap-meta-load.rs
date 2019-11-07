use std::collections::HashMap;
use std::env;
use std::str;

use csv;
use email_parser::Message;
use postgres::{Client, NoTls};
use serde_derive::{Deserialize, Serialize};

fn main() {
    let mut args = env::args();
    let map = read_meta(&args.nth(1).unwrap());
    println!("metadata for {} messages found", map.len());
    let conn = Client::connect("postgres://postgres@localhost:5432/mail-djc", NoTls).unwrap();
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

fn sender_address(s: &str) -> &str {
    let start = match s.find("<") {
        Some(s) => s,
        None => {
            return s.trim();
        }
    };
    let started = &s[start + 1..];
    let end = started.find(">").expect("'>' must be present in sender");
    &started[..end]
}

fn process(map: MetaMap, mut conn: Client) {
    let mut i = 0;
    let stmt = conn
        .prepare("UPDATE messages SET unid = $1, mod_seq = $2 WHERE id = $3")
        .unwrap();
    for row in &conn
        .query(
            "SELECT id, dt, mid, subject, raw FROM messages WHERE unid IS NULL ORDER BY id ASC",
            &[],
        )
        .unwrap()
    {
        if i % 10000 == 0 {
            println!("processed {} messages", i);
        }
        i += 1;

        let id: i64 = row.get(0);
        let mid: Option<String> = row.get(2);
        let subject: Option<String> = row.get(3);
        if mid.is_none() {
            //println!("no Message-ID for {}, skipping", id);
            continue;
        }

        let mid = mid.unwrap();
        if mid.ends_with("chat@gmail.com>") {
            continue;
        }

        let metas = match map.get(&mid) {
            Some(m) => m,
            None => {
                if mid.len() > 5 {
                    match conn.execute("DELETE FROM messages WHERE id = $1", &[&id]) {
                        Ok(_) => {}
                        Err(e) => {
                            println!("error while deleting message with id {}: {:?}", id, e);
                        }
                    }
                }
                continue;
            }
        };

        let metas: Vec<&MessageMeta> = metas
            .iter()
            .filter(|m| match (&m.subject, &subject) {
                (&Some(ref meta_subj), &Some(ref db_subj)) => {
                    if meta_subj.len() == 998 {
                        (meta_subj as &str) == &db_subj[..998]
                    } else {
                        meta_subj == db_subj
                    }
                }
                (&Some(_), &None) | (&None, &Some(_)) => false,
                (&None, &None) => true,
            })
            .collect();
        if metas.is_empty() {
            println!(
                "no matches left after filtering based on subject = {:?}",
                subject
            );
        }

        let raw: String = row.get(4);
        let msg = Message::from_slice(raw.as_bytes());
        let headers = msg.headers();
        let mut sender = headers.get_first("sender");
        if sender.is_none() {
            sender = headers.get_first("from");
        }

        let metas: Vec<&&MessageMeta> = metas
            .iter()
            .filter(|m| {
                println!("senders {:?} {:?}", m.sender, sender);
                match (&m.sender, &sender) {
                    (&Some(ref meta_sender), &Some(ref db_sender)) => {
                        let ms = sender_address(meta_sender);
                        let ds = sender_address(db_sender);
                        ms == ds
                    }
                    (&Some(_), &None) | (&None, &Some(_)) => false,
                    (&None, &None) => true,
                }
            })
            .collect();
        if metas.is_empty() {
            println!(
                "no matches left after filtering based on sender = {:?}",
                sender
            );
        }

        println!("from {:?}", headers.get_first("from"));
        println!("sent to {:?}", headers.get_first("to"));

        if metas.len() == 1 {
            let meta = metas.get(0).unwrap();
            match conn.execute(&stmt, &[&(meta.uid as i64), &(meta.mod_seq as i64), &id]) {
                Ok(num) => println!("updated {} rows", num),
                Err(e) => println!("update result {:?}", e),
            }
            continue;
        } else if metas.len() > 1 {
            let meta = metas.get(0).unwrap();
            println!(
                "multiple matches for {} = {} (subject = {:?})",
                mid,
                metas.len(),
                meta.subject
            );
        } else {
            println!("no matches left for {} (subject = {:?})", mid, subject);
        }
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
    subject: Option<String>,
    sender: Option<String>,
}
