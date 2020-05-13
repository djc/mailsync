use std::collections::HashMap;
use std::fs;
use std::io;

use chrono::{DateTime, FixedOffset};
use postgres_types::{FromSql, ToSql};
use serde_derive::{Deserialize, Serialize};
use tokio_imap::builders::{fetch, FetchCommand};
use tokio_imap::ResponseData;
use tokio_imap::types::{Attribute, AttributeValue, Response};

#[derive(Default)]
pub struct ResponseAccumulator {
    parts: HashMap<u32, (u32, Vec<ResponseData>)>,
}

impl ResponseAccumulator {
    pub fn new() -> ResponseAccumulator {
        Self::default()
    }

    pub fn build_command_attributes(
        builder: FetchCommand<fetch::Messages>,
    ) -> FetchCommand<fetch::Attributes> {
        builder
            .attr(Attribute::Uid)
            .attr(Attribute::ModSeq)
            .attr(Attribute::InternalDate)
            .attr(Attribute::Rfc822)
            .attr(Attribute::Flags)
    }

    pub fn push(mut self, rd: ResponseData) -> (Self, Option<MessageMeta>) {
        use crate::AttributeValue::*;

        let completed = {
            let (_, entry) = match *rd.parsed() {
                Response::Fetch(idx, ref attr_vals) => {
                    let mut entry = self.parts.entry(idx).or_insert((0, vec![]));
                    for val in attr_vals.iter() {
                        entry.0 += match *val {
                            Uid(_) | ModSeq(_) | InternalDate(_) | Flags(_) | Rfc822(_) => 1,
                            _ => 0,
                        };
                    }
                    (idx, entry)
                }
                _ => return (self, None),
            };
            entry.1.push(rd);

            // If we think all the parts are in, extract the relevant content
            // into a single structure
            if entry.0 == 5 {
                let mut seq = None;
                let mut uid = None;
                let mut mod_seq = None;
                let mut dt = None;
                let mut flags = Vec::new();
                let mut source = None;
                for rd in entry.1.drain(..) {
                    if let Response::Fetch(rsp_seq, attr_vals) = rd.parsed() {
                        seq = Some(*rsp_seq);
                        for val in attr_vals.iter() {
                            match *val {
                                Uid(u) => {
                                    uid = Some(u);
                                }
                                ModSeq(ms) => {
                                    mod_seq = Some(ms);
                                }
                                InternalDate(id) => {
                                    let parsed =
                                        DateTime::parse_from_str(id, "%d-%b-%Y %H:%M:%S %z");
                                    dt = Some(parsed.unwrap());
                                }
                                Flags(ref fs) => {
                                    flags.extend(fs.iter().filter_map(|&f| Flag::from_str(f)));
                                }
                                Rfc822(Some(src)) => {
                                    source = Some(src.to_vec());
                                }
                                _ => {}
                            }
                        }
                    };
                }
                Some(MessageMeta {
                    seq: seq.unwrap(),
                    uid: uid.unwrap(),
                    mod_seq: mod_seq.unwrap(),
                    dt: dt.unwrap(),
                    flags,
                    raw: source.unwrap(),
                })
            } else {
                None
            }
        };

        // Once we have found a complete Meta struct, remove the related messages
        // from the internal parts cache and return the Meta struct.
        if let Some(ref meta) = completed {
            self.parts.remove(&meta.seq);
        }
        (self, completed)
    }
}

pub fn fuzzy_datetime_parser(orig: &str) -> Option<DateTime<FixedOffset>> {
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
        .split(' ')
        .take(5)
        .map(|s| s.to_string())
        .collect::<Vec<String>>();
    if parts.len() > 3 && parts[3].contains(".") {
        let mut t = parts.remove(3);
        t = t.replace('.', ":");
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

const FORMATS: [&str; 5] = [
    "%-e %b %Y %H:%M:%S %z",
    "%-e %b %Y %H:%M %z",
    "%e %b %Y %H:%M:%S %z",
    "%e %b %Y %H:%M",
    "%b %d %H:%M:%S %z %Y",
];

#[derive(Debug, Deserialize, Serialize)]
pub struct MessageMeta {
    pub seq: u32,
    pub uid: u32,
    pub mod_seq: u64,
    pub dt: DateTime<FixedOffset>,
    pub flags: Vec<Flag>,
    pub raw: Vec<u8>,
}

#[derive(Debug, Deserialize, FromSql, PartialEq, Serialize, ToSql)]
#[postgres(name = "flags")]
pub enum Flag {
    #[postgres(name = "\\Answered")]
    Answered,
    #[postgres(name = "\\Flagged")]
    Flagged,
    #[postgres(name = "\\Seen")]
    Seen,
}

impl Flag {
    pub fn from_str(s: &str) -> Option<Flag> {
        match s {
            "\\Answered" => Some(Flag::Answered),
            "\\Flagged" => Some(Flag::Flagged),
            "\\Seen" => Some(Flag::Seen),
            "Junk" | "$Phishing" | "NonJunk" | "$MDNSent" | "$Forwarded" => None,
            v => {
                println!("unknown flag: {}", v);
                None
            }
        }
    }
}

#[derive(Deserialize)]
pub struct Config {
    pub imap: ImapConfig,
    pub store: StoreConfig,
}

impl Config {
    pub fn from_file(name: &str) -> Self {
        let s = fs::read_to_string(name).unwrap();
        toml::from_str(&s).unwrap()
    }
}

#[derive(Deserialize)]
pub struct ImapConfig {
    pub server: String,
    pub account: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct StoreConfig {
    pub uri: String,
}

pub struct Context {
    pub client: tokio_imap::TlsClient,
    pub db: tokio_postgres::Client,
}

#[derive(Debug)]
pub struct Label {
    pub id: i32,
    pub name: String,
    pub mod_seq: Option<i64>,
}

macro_rules! error_enum_impls {
    ($name:ident, $( $variant:ident : $ty:path ),+ ) => {
        $(impl From<$ty> for $name {
            fn from(e: $ty) -> Self {
                $name:: $variant (e)
            }
        })*
    };
}

macro_rules! error_enum {
    ($name:ident, $( $variant:ident : $ty:path ),+ $(,)* ) => {
        #[derive(Debug)]
        pub enum $name {
            $($variant($ty)),*
        }
        error_enum_impls!($name, $($variant : $ty),*);
    };
}

error_enum!(SyncError, Io: io::Error, Pg: tokio_postgres::error::Error,);
