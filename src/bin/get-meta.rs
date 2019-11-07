extern crate csv;
extern crate futures;
extern crate futures_state_stream;
extern crate mailsync;
#[macro_use]
extern crate serde_derive;
extern crate tokio_core;
extern crate tokio_imap;
extern crate toml;

use futures::future::{ok, Future};
use futures_state_stream::StateStream;
use mailsync::*;
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::Read;
use std::str;
use tokio_core::reactor::Core;
use tokio_imap::client::builder::*;
use tokio_imap::proto::*;
use tokio_imap::types::{Attribute, AttributeValue, MailboxDatum, Response};
use tokio_imap::{ImapClient, TlsClient};

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut f = File::open(&args[1]).unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();
    let config: Config = toml::from_str(&s).unwrap();

    let mut core = Core::new().unwrap();
    let mut out = csv::Writer::from_path("imap-meta.csv").unwrap();
    core.run(
        TlsClient::connect(&config.imap.server)
            .unwrap()
            .map_err(|e| SyncError::from(e))
            .and_then(|(_, client)| {
                client
                    .call(CommandBuilder::login(
                        &config.imap.account,
                        &config.imap.password,
                    ))
                    .collect()
                    .map_err(|e| SyncError::from(e))
            })
            .and_then(|(_, client)| get_metadata(client, &mut out)),
    )
    .unwrap();
}

fn get_metadata<'a>(
    client: TlsClient,
    writer: &'a mut csv::Writer<std::fs::File>,
) -> Box<Future<Item = TlsClient, Error = SyncError> + 'a> {
    Box::new(
        client
            .call(CommandBuilder::examine("[Gmail]/All Mail"))
            .collect()
            .map_err(|e| SyncError::from(e))
            .and_then(|(msgs, client)| {
                let exists = msgs
                    .iter()
                    .filter_map(|rd| match *rd.parsed() {
                        Response::MailboxData(MailboxDatum::Exists(num)) => Some(num),
                        _ => None,
                    })
                    .nth(0)
                    .unwrap();
                let (start, end) = (1, exists);
                println!("fetch metadata for {}:{}", start, end);
                let cmd = CommandBuilder::fetch()
                    .range(start, end)
                    .attr(Attribute::Uid)
                    .attr(Attribute::ModSeq)
                    .attr(Attribute::Flags)
                    .attr(Attribute::Envelope)
                    .build();
                client.call(cmd).map_err(|e| SyncError::from(e)).fold(
                    ResponseAccumulator::new(4),
                    move |acc, rd| {
                        let (new, meta_opt) = acc.push(rd);
                        if let Some(meta) = meta_opt {
                            if meta.seq % 1000 == 0 {
                                println!("store metadata for index {}", meta.seq);
                            }
                            writer.serialize(meta).unwrap();
                        }
                        ok::<ResponseAccumulator, SyncError>(new)
                    },
                )
            })
            .and_then(|(_, client)| {
                client
                    .call(CommandBuilder::close())
                    .collect()
                    .map_err(|e| SyncError::from(e))
                    .and_then(|(_, client)| ok(client))
            }),
    )
}

struct ResponseAccumulator {
    parts: HashMap<u32, (u32, Vec<ResponseData>)>,
    num_parts: u32,
}

impl ResponseAccumulator {
    fn new(num_parts: u32) -> ResponseAccumulator {
        ResponseAccumulator {
            parts: HashMap::new(),
            num_parts,
        }
    }
    fn push(mut self, rd: ResponseData) -> (Self, Option<MessageMeta>) {
        use AttributeValue::*;
        let completed = {
            let (idx, mut entry) = match *rd.parsed() {
                Response::Fetch(idx, ref attr_vals) => {
                    let mut entry = self.parts.entry(idx).or_insert((0, vec![]));
                    for val in attr_vals.iter() {
                        entry.0 += match *val {
                            Uid(_) | ModSeq(_) | Flags(_) | Envelope(_) => 1,
                            _ => 0,
                        };
                    }
                    (idx, entry)
                }
                _ => return (self, None),
            };
            entry.1.push(rd);
            if entry.0 == self.num_parts {
                let mut mod_seq = None;
                let mut uid = None;
                let mut mid = None;
                let mut dt = None;
                let mut subject = None;
                let mut sender = None;
                let mut flags = None;
                for rd in entry.1.drain(..) {
                    match *rd.parsed() {
                        Response::Fetch(_, ref attr_vals) => {
                            for val in attr_vals.iter() {
                                match *val {
                                    Uid(u) => {
                                        uid = Some(u);
                                    }
                                    ModSeq(ms) => {
                                        mod_seq = Some(ms);
                                    }
                                    Flags(ref fs) => {
                                        let list: Vec<&str> = fs.iter().map(|f| *f).collect();
                                        flags = Some(list.join(" "));
                                    }
                                    Envelope(ref env) => {
                                        mid = env.message_id.map(|r| r.to_string());
                                        dt = env.date.map(|r| r.to_string());
                                        subject = env.subject.map(|r| r.to_string());
                                        if let Some(ref senders) = env.sender {
                                            sender = Some(format!(
                                                "{} <{}@{}>",
                                                senders[0].name.unwrap_or(""),
                                                senders[0].mailbox.unwrap_or(""),
                                                senders[0].host.unwrap_or("")
                                            ));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    };
                }
                Some(MessageMeta {
                    seq: idx,
                    uid: uid.unwrap(),
                    mod_seq: mod_seq.unwrap(),
                    flags: flags.unwrap(),
                    mid: mid,
                    date: dt,
                    subject,
                    sender,
                })
            } else {
                None
            }
        };
        if let Some(ref meta) = completed {
            self.parts.remove(&meta.seq);
        }
        (self, completed)
    }
}

#[derive(Deserialize, Serialize)]
struct MessageMeta {
    seq: u32,
    uid: u32,
    mod_seq: u64,
    flags: String,
    mid: Option<String>,
    date: Option<String>,
    subject: Option<String>,
    sender: Option<String>,
}
