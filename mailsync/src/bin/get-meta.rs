use std::collections::HashMap;
use std::str;

use bincode;
use chrono::{DateTime, FixedOffset};
use futures::future::ok;
use serde_derive::{Deserialize, Serialize};
use sled;
use structopt::StructOpt;
use tokio_imap::client::builder::CommandBuilder;
use tokio_imap::proto::ResponseData;
use tokio_imap::types::{Attribute, AttributeValue, MailboxDatum, Response};
use tokio_imap::TlsClient;

use mailsync::{fuzzy_datetime_parser, Config, Flag};

#[tokio::main]
async fn main() {
    let options = Options::from_args();
    let config = Config::from_file(&options.config);

    let db = sled::open("mail.sled").unwrap();
    let tree = db.open_tree("meta").unwrap();
    let mut wrote = (0usize, 0usize);

    let (_, mut client) = TlsClient::connect(&config.imap.server).await.unwrap();
    let _ = client
        .call(CommandBuilder::login(
            &config.imap.account,
            &config.imap.password,
        ))
        .try_collect()
        .await
        .unwrap();

    let msgs = client
        .call(CommandBuilder::examine("[Gmail]/All Mail"))
        .try_collect()
        .await
        .unwrap();
    let exists = msgs
        .iter()
        .filter_map(|rd| match rd.parsed() {
            Response::MailboxData(MailboxDatum::Exists(num)) => Some(*num),
            _ => None,
        })
        .next()
        .unwrap();

    println!("{} messages found, fetching metadata...", exists);
    let cmd = CommandBuilder::fetch()
        .range_from(1..)
        .attr(Attribute::Uid)
        .attr(Attribute::ModSeq)
        .attr(Attribute::Flags)
        .attr(Attribute::Envelope);

    let _ = client
        .call(cmd)
        .try_fold(ResponseAccumulator::new(4), |acc, rd| {
            let (new, meta_opt) = acc.push(rd);
            if let Some(meta) = meta_opt {
                if meta.seq % 1000 == 0 {
                    println!("store metadata for index {}", meta.seq);
                }

                let key = meta.uid.to_be_bytes();
                let serialized = bincode::serialize(&meta).unwrap();
                wrote.0 += 1;
                wrote.1 += serialized.len();
                tree.insert(&key, serialized).unwrap();
            }
            ok(new)
        })
        .await
        .unwrap();

    let _ = client
        .call(CommandBuilder::close())
        .try_collect()
        .await
        .unwrap();

    println!("wrote {} items, total size {}", wrote.0, wrote.1);
}

#[derive(Debug, StructOpt)]
#[structopt(name = "get-meta")]
struct Options {
    config: String,
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
        use crate::AttributeValue::*;
        let (idx, entry) = match *rd.parsed() {
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
        if entry.0 < self.num_parts {
            return (self, None);
        }

        let mut mod_seq = None;
        let mut uid = None;
        let mut mid = None;
        let mut dt = None;
        let mut subject = None;
        let mut sender = None;
        let mut flags = Vec::new();
        for rd in entry.1.drain(..) {
            if let Response::Fetch(_, attr_vals) = rd.parsed() {
                for val in attr_vals.iter() {
                    match *val {
                        Uid(u) => {
                            uid = Some(u);
                        }
                        ModSeq(ms) => {
                            mod_seq = Some(ms);
                        }
                        Flags(ref fs) => {
                            flags.extend(fs.iter().filter_map(|f| Flag::from_str(f)));
                        }
                        Envelope(ref env) => {
                            mid = env.message_id.map(|r| String::from_utf8_lossy(r).into());
                            dt = env
                                .date
                                .and_then(|r| fuzzy_datetime_parser(str::from_utf8(r).unwrap()));

                            if dt.is_none() {
                                if let Some(dt) = env.date {
                                    println!(
                                        "failed to parse date: {}",
                                        str::from_utf8(dt).unwrap()
                                    );
                                }
                            }

                            subject = env.subject.map(|r| String::from_utf8_lossy(r).into());
                            if let Some(ref senders) = env.sender {
                                sender = Some(format!(
                                    "{} <{}@{}>",
                                    String::from_utf8_lossy(senders[0].name.unwrap_or(b"")),
                                    String::from_utf8_lossy(senders[0].mailbox.unwrap_or(b"")),
                                    String::from_utf8_lossy(senders[0].host.unwrap_or(b"")),
                                ));
                            }
                        }
                        _ => {}
                    }
                }
            };
        }

        self.parts.remove(&idx);
        (
            self,
            Some(MessageMeta {
                seq: idx,
                uid: uid.unwrap(),
                mod_seq: mod_seq.unwrap(),
                flags,
                mid,
                dt,
                subject,
                sender,
            }),
        )
    }
}

#[derive(Deserialize, Serialize)]
struct MessageMeta {
    seq: u32,
    uid: u32,
    mod_seq: u64,
    flags: Vec<Flag>,
    mid: Option<String>,
    dt: Option<DateTime<FixedOffset>>,
    subject: Option<String>,
    sender: Option<String>,
}
