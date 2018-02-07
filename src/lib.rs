extern crate chrono;
extern crate futures;
extern crate futures_state_stream;
#[macro_use]
extern crate postgres;
#[macro_use]
extern crate postgres_derive;
#[macro_use]
extern crate serde_derive;
extern crate toml;
extern crate tokio_core;
extern crate tokio_imap;
extern crate tokio_postgres;

use chrono::{DateTime, FixedOffset};

use futures::future::Future;

use std::collections::HashMap;
use std::io;

use tokio_imap::proto::*;
use tokio_imap::types::*;
use tokio_imap::client::builder::*;

use tokio_postgres::Connection;


pub struct ResponseAccumulator {
    parts: HashMap<u32, (u32, Vec<ResponseData>)>,
}

impl ResponseAccumulator {
    pub fn new() -> ResponseAccumulator {
        ResponseAccumulator {
            parts: HashMap::new(),
        }
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
        use AttributeValue::*;

        let completed = {
            let (_, mut entry) = match *rd.parsed() {
                Response::Fetch(idx, ref attr_vals) => {
                    let mut entry = self.parts.entry(idx).or_insert((0, vec![]));
                    for val in attr_vals.iter() {
                        entry.0 += match *val {
                            Uid(_) |
                            ModSeq(_) |
                            InternalDate(_) |
                            Flags(_) |
                            Rfc822(_) => 1,
                            _ => 0,
                        };
                    }
                    (idx, entry)
                },
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
                    match *rd.parsed() {
                        Response::Fetch(rsp_seq, ref attr_vals) => {
                            seq = Some(rsp_seq);
                            for val in attr_vals.iter() {
                                match *val {
                                    Uid(u) => {
                                        uid = Some(u);
                                    },
                                    ModSeq(ms) => {
                                        mod_seq = Some(ms);
                                    },
                                    InternalDate(id) => {
                                        let parsed = DateTime::parse_from_str(id, "%d-%b-%Y %H:%M:%S %z");
                                        dt = Some(parsed.unwrap());
                                    },
                                    Flags(ref fs) => {
                                        flags = Some(fs.iter().map(|s| (*s).into()).collect::<Vec<Flag>>());
                                    },
                                    Rfc822(Some(src)) => {
                                        source = Some(src.to_vec());
                                    },
                                    _ => {},
                                }
                            }
                        },
                        _ => {},
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

pub type ContextFuture = Future<Item = Context, Error = SyncError>;

pub struct Context {
    pub client: tokio_imap::Client,
    pub conn: Connection,
}

#[derive(Debug)]
pub struct Label {
    pub id: i32,
    pub name: String,
    pub mod_seq: Option<i64>,
}

impl From<(tokio_postgres::Error, Connection)> for SyncError {
    fn from(e: (tokio_postgres::Error, Connection)) -> Self {
        let (e, _) = e;
        SyncError::from(e)
    }
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

error_enum!(SyncError,
    Io: io::Error,
    Pg: tokio_postgres::error::Error,
);
