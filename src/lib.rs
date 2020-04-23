use std::collections::HashMap;
use std::fs;
use std::io;

use chrono::{DateTime, FixedOffset};
use postgres_types::{FromSql, ToSql};
use serde_derive::{Deserialize, Serialize};
use tokio_imap::client::builder::{
    FetchBuilderAttributes, FetchCommandAttributes, FetchCommandMessages,
};
use tokio_imap::proto::ResponseData;
use tokio_imap::types::{Attribute, AttributeValue, Response};

#[derive(Default)]
pub struct ResponseAccumulator {
    parts: HashMap<u32, (u32, Vec<ResponseData>)>,
}

impl ResponseAccumulator {
    pub fn new() -> ResponseAccumulator {
        Self::default()
    }

    pub fn build_command_attributes(builder: FetchCommandMessages) -> FetchCommandAttributes {
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
                let mut flags = None;
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
                                    flags =
                                        Some(fs.iter().map(|s| (*s).into()).collect::<Vec<Flag>>());
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
                    flags: flags.unwrap(),
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

#[derive(Debug, Deserialize, Serialize)]
pub struct MessageMeta {
    pub seq: u32,
    pub uid: u32,
    pub mod_seq: u64,
    pub dt: DateTime<FixedOffset>,
    pub flags: Vec<Flag>,
    pub raw: Vec<u8>,
}

#[derive(Debug, Deserialize, FromSql, Serialize, ToSql)]
#[postgres(name = "flags")]
pub enum Flag {
    #[postgres(name = "\\Answered")]
    Answered,
    #[postgres(name = "\\Flagged")]
    Flagged,
    #[postgres(name = "\\Seen")]
    Seen,
}

impl<'a> From<&'a str> for Flag {
    fn from(s: &'a str) -> Self {
        match s {
            "\\Answered" => Flag::Answered,
            "\\Flagged" => Flag::Flagged,
            "\\Seen" => Flag::Seen,
            _ => panic!("unsupported flag value '{}'", s),
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
