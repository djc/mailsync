extern crate futures;
extern crate futures_state_stream;
#[macro_use]
extern crate serde_derive;
extern crate toml;
extern crate tokio_core;
extern crate tokio_imap;
extern crate tokio_postgres;

use futures::future::{Future, ok};
use futures::stream::{self, Stream};
use futures_state_stream::StateStream;
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{self, Read};
use std::str;
use tokio_core::reactor::Core;
use tokio_imap::proto::*;
use tokio_imap::client::builder::*;
use tokio_postgres::{Connection, TlsMode};

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut f = File::open(&args[1]).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();
    let config: Config = toml::from_str(&s).unwrap();

    let mut core = Core::new().unwrap();
    let handle = core.handle();
    core.run(
        tokio_imap::Client::connect(&config.imap.server, &handle)
            .map_err(|e| SyncError::from(e))
            .and_then(|(client, _)| {
                client
                    .call(CommandBuilder::login(
                        &config.imap.account,
                        &config.imap.password,
                    ))
                    .collect()
                    .map_err(|e| SyncError::from(e))
            })
            .join(
                Connection::connect(config.store.uri.clone(), TlsMode::None, &handle)
                    .map_err(|e| SyncError::from(e))
            )
            .and_then(|((_, client), conn)| check_labels(Context { client, conn })),
    ).unwrap();
}

fn check_labels(ctx: Context) -> Box<ContextFuture> {
    let Context { client, conn } = ctx;
    Box::new(
        conn.prepare("SELECT id, name, mod_seq FROM labels")
            .and_then(|(stmt, conn)| {
                conn.query(&stmt, &[])
                    .map(|row| {
                        Ok(Label {
                            id: row.get(0),
                            name: row.get(1),
                            mod_seq: row.get(2),
                        })
                    })
                    .collect()
            })
            .map_err(|e| SyncError::from(e))
            .and_then(|(labels, conn)| {
                stream::iter(labels).fold(Context { client, conn }, |ctx, label| {
                    if label.mod_seq.is_some() {
                        println!("start sync for label {} (from {:?})",
                            &label.name, &label.mod_seq);
                        sync_label(ctx, &label)
                    } else {
                        println!("load messages for label {}", &label.name);
                        load_label(ctx, &label)
                    }
                })
            })
            .and_then(ok),
    )
}

fn sync_label(ctx: Context, label: &Label) -> Box<ContextFuture> {
    let Context { client, conn } = ctx;
    Box::new(
        client.call(CommandBuilder::select(&label.name))
            .collect()
            .and_then(|(_, client)| {
                let cmd = CommandBuilder::fetch()
                    .all_after(1)
                    .attr(Attribute::Envelope)
                    .changed_since(29248804)
                    .build();
                client.call(cmd).for_each(|rd| {
                    println!("server: {:?}", &rd.parsed());
                    Ok(())
                })
            })
            .and_then(|client| client.call(CommandBuilder::close()).collect())
            .and_then(|(_, client)| ok(Context { client, conn }))
            .map_err(|e| SyncError::from(e))
    )
}

fn load_label(ctx: Context, label: &Label) -> Box<ContextFuture> {
    let Context { client, conn } = ctx;
    Box::new(
        client.call(CommandBuilder::select(&label.name))
            .collect()
            .map_err(|e| SyncError::from(e))
            .and_then(|(label_meta, client)| {
                let exists = label_meta.iter().filter_map(|rd| match *rd.parsed() {
                    Response::MailboxData(MailboxDatum::Exists(num)) => Some(num),
                    _ => None,
                }).nth(0).unwrap();
                let cmd = CommandBuilder::fetch()
                    .num(exists)
                    .attr(Attribute::Uid)
                    .attr(Attribute::ModSeq)
                    .attr(Attribute::InternalDate)
                    .attr(Attribute::Rfc822)
                    .build();
                client.call(cmd)
                    .map_err(|e| SyncError::from(e))
                    .fold(
                         ResponseAccumulator::new(4),
                         |acc, rd| ok::<ResponseAccumulator, SyncError>(acc.push(rd)),
                    )
            })
            .join(conn.prepare("INSERT INTO messages (unid, mod_seq, raw)\
                                VALUES ($1, $2, $3)")
                .map_err(|e| SyncError::from(e)))
            .and_then(|((acc, client), (stmt, conn))| {
                let ResponseAccumulator { parts, mut full, .. } = acc;
                assert_eq!(parts.len(), 0);
                let metas: Vec<Result<MessageMeta, SyncError>> = full.drain().map(
                    |(_, v)| Ok(v)
                ).collect();
                stream::iter(metas).fold((stmt, conn),
                    |(stmt, conn), meta| {
                        let raw = match *meta.raw.parsed() {
                            Response::Fetch(_, ref attr_vals) => {
                                let val = attr_vals.iter().find(|val|
                                    match **val {
                                        AttributeValue::Rfc822(_) => true,
                                        _ => false,
                                    }
                                );
                                match val {
                                    Some(&AttributeValue::Rfc822(raw)) => Some(raw),
                                    _ => None,
                                }
                            },
                            _ => None,
                        };
                        println!("execute {} {}!", meta.uid, meta.mod_seq);
                        conn.execute(&stmt, &[
                            &(meta.uid as i64),
                            &(meta.mod_seq as i64),
                            &raw.unwrap(),
                        ])
                        .join(ok(stmt))
                        .and_then(|((rows, conn), stmt)| {
                            println!("{} rows", rows);
                            ok((stmt, conn))
                        })
                    }
                ).join(ok(client))
            })
            .and_then(|((_, conn), client)| {
                client.call(CommandBuilder::close()).collect()
                    .join(ok(conn))
                    .map_err(|e| SyncError::from(e))
            })
            .and_then(|((_, client), conn)| ok(Context { client, conn }))
    )
}

struct ResponseAccumulator {
    parts: HashMap<u32, (u32, Vec<ResponseData>)>,
    full: HashMap<u32, MessageMeta>,
    num_parts: u32,
}

impl ResponseAccumulator {
    fn new(num_parts: u32) -> ResponseAccumulator {
        ResponseAccumulator {
            parts: HashMap::new(),
            full: HashMap::new(),
            num_parts,
        }
    }
    fn push(mut self, rd: ResponseData) -> Self {
        use AttributeValue::*;
        let completed = {
            let (idx, mut entry) = match *rd.parsed() {
                Response::Fetch(idx, ref attr_vals) => {
                    let mut entry = self.parts.entry(idx).or_insert((0, vec![]));
                    for val in attr_vals.iter() {
                        entry.0 += match *val {
                            Uid(_) |
                            ModSeq(_) |
                            InternalDate(_) |
                            Rfc822(_) => 1,
                            _ => 0,
                        };
                    }
                    (idx, entry)
                },
                _ => return self,
            };
            entry.1.push(rd);
            if entry.0 == self.num_parts {
                let mut date = None;
                let mut mod_seq = None;
                let mut uid = None;
                let mut raw = None;
                for rd in entry.1.drain(..) {
                    let has_source = match *rd.parsed() {
                        Response::Fetch(_, ref attr_vals) => {
                            let mut rsp_source = false;
                            for val in attr_vals.iter() {
                                rsp_source |= match *val {
                                    Uid(u) => {
                                        uid = Some(u);
                                        false
                                    },
                                    ModSeq(ms) => {
                                        mod_seq = Some(ms);
                                        false
                                    },
                                    InternalDate(id) => {
                                        date = Some(id.to_string());
                                        false
                                    },
                                    Rfc822(_) => true,
                                    _ => false,
                                }
                            }
                            rsp_source
                        },
                        _ => false,
                    };
                    if has_source {
                        raw = Some(rd);
                    }
                }
                Some((idx, MessageMeta {
                    uid: uid.unwrap(),
                    mod_seq: mod_seq.unwrap(),
                    date: date.unwrap(),
                    raw: raw.unwrap(),
                }))
            } else {
                None
            }
        };
        if let Some((idx, meta)) = completed {
            self.parts.remove(&idx);
            self.full.insert(idx, meta);
        }
        return self;
    }
}

struct MessageMeta {
    uid: u32,
    mod_seq: u64,
    date: String,
    raw: ResponseData,
}

#[derive(Deserialize)]
struct Config {
    imap: ImapConfig,
    store: StoreConfig,
}

#[derive(Deserialize)]
struct ImapConfig {
    server: String,
    account: String,
    password: String,
}

#[derive(Deserialize)]
struct StoreConfig {
    uri: String,
}

type ContextFuture = Future<Item = Context, Error = SyncError>;

struct Context {
    client: tokio_imap::Client,
    conn: Connection,
}

#[derive(Debug)]
struct Label {
    id: i32,
    name: String,
    mod_seq: Option<i64>,
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
        enum $name {
            $($variant($ty)),*
        }
        error_enum_impls!($name, $($variant : $ty),*);
    };
}

error_enum!(SyncError,
    Io: io::Error,
    Pg: tokio_postgres::error::Error,
    PgConnect: tokio_postgres::error::ConnectError,
);
